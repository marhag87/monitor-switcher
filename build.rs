//! Build glue for libCEC on Windows.
//!
//! Two things the Pulse-Eight installer leaves us to sort out:
//!
//! 1. It ships `cec.dll` and the headers but no import library, which the MSVC
//!    linker needs. We generate one from the DLL's own export table. Nothing
//!    derived from libCEC is committed to this repo as a result — the import
//!    library is rebuilt from whatever is installed, which also means it cannot
//!    drift out of step with the DLL.
//! 2. It does not put its directory on PATH, and libCEC is linked at load time,
//!    so `cec.dll` has to sit next to the executable for a shortcut launched
//!    from anywhere to work.

use std::path::{Path, PathBuf};
use std::process::Output;

const DEFAULT_LIBCEC_DIR: &str = r"C:\Program Files\Pulse-Eight\USB-CEC Adapter";
const TARGET: &str = "x86_64-pc-windows-msvc";

fn main() {
    println!("cargo:rerun-if-env-changed=LIBCEC_BIN_DIR");

    // Nothing to do without the `cec` feature — and importantly, no libCEC
    // needs to be installed for a default build to succeed.
    if std::env::var_os("CARGO_FEATURE_CEC").is_none() {
        return;
    }

    let dir = std::env::var("LIBCEC_BIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_LIBCEC_DIR));
    let dll = dir.join("cec.dll");
    println!("cargo:rerun-if-changed={}", dll.display());

    if !dll.exists() {
        println!(
            "cargo:warning=cec.dll not found at {} — install libCEC x64, or set LIBCEC_BIN_DIR",
            dll.display()
        );
        return;
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    generate_import_library(&dll, &out);
    place_dll_next_to_executable(&dll, &out);
}

/// Build `cec.lib` from the DLL's exports and point the linker at it.
///
/// This takes precedence over the search path the libcec crate emits (which
/// points at the install directory, where there is no import library).
///
/// Both tools are run through the `Command` `windows_registry::find` hands back,
/// rather than a fresh one built from its program path. That `Command` arrives
/// with `PATH`, `LIB` and `INCLUDE` already pointing at the matching MSVC and
/// Windows Kits directories, and dumpbin and lib are front-ends that load their
/// real implementation from those directories at runtime. Keeping only the
/// program path happens to work where every dependency sits next to the
/// executable — Windows searches there first — and fails wherever one does not.
fn generate_import_library(dll: &Path, out: &Path) {
    let Some(mut dumpbin) = cc::windows_registry::find(TARGET, "dumpbin.exe") else {
        println!("cargo:warning=dumpbin.exe not found; cannot generate cec.lib");
        return;
    };
    let Some(mut lib_exe) = cc::windows_registry::find(TARGET, "lib.exe") else {
        println!("cargo:warning=lib.exe not found; cannot generate cec.lib");
        return;
    };

    let exports = dumpbin
        .arg("/exports")
        .arg(dll)
        .output()
        .expect("running dumpbin");
    // `output()` returning Ok only means the process ran. Skipping this check
    // let a failed dumpbin through with empty stdout, which parsed to zero
    // exports and then reported itself as an unreadable DLL — while dumpbin's
    // own account of the problem was discarded.
    assert!(
        exports.status.success(),
        "dumpbin could not read {} ({})\n{}",
        dll.display(),
        exports.status,
        tool_output(&exports)
    );
    let text = String::from_utf8_lossy(&exports.stdout);

    let mut def = String::from("EXPORTS\n");
    let mut count = 0;
    // Rows look like: `  1    0 0001D570 CECDestroy`, after a header line
    // ending in "name". Anything with four fields whose first is an ordinal is
    // an export.
    for line in text.lines().skip_while(|l| !l.trim_end().ends_with("name")) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if let [ordinal, _hint, _rva, name] = fields.as_slice() {
            if ordinal.chars().all(|c| c.is_ascii_digit()) {
                def.push_str(name);
                def.push('\n');
                count += 1;
            }
        }
    }
    // Distinct from the failure above: dumpbin was happy, so this is either not
    // the DLL we think it is or a change in the output format the parser above
    // relies on. Either way the next question is what dumpbin actually printed.
    assert!(
        count > 0,
        "dumpbin read {} but no exports were found in its output.\nWhat it printed:\n{}",
        dll.display(),
        text.lines().take(20).collect::<Vec<_>>().join("\n")
    );

    let def_path = out.join("cec.def");
    std::fs::write(&def_path, def).expect("writing cec.def");

    // Captured rather than inherited, so a successful run stays quiet and a
    // failed one has its reason attached to the panic instead of scrolling past
    // in the build log.
    let lib_run = lib_exe
        .arg(format!("/def:{}", def_path.display()))
        .arg("/machine:x64")
        .arg(format!("/out:{}", out.join("cec.lib").display()))
        .output()
        .expect("running lib.exe");
    assert!(
        lib_run.status.success(),
        "lib.exe could not build cec.lib from {} ({})\n{}",
        def_path.display(),
        lib_run.status,
        tool_output(&lib_run)
    );

    println!("cargo:rustc-link-search=native={}", out.display());
}

/// Everything a tool printed, whichever stream it chose: lib.exe reports its
/// errors on stdout, dumpbin on stderr.
fn tool_output(out: &Output) -> String {
    [&out.stdout, &out.stderr]
        .into_iter()
        .map(|stream| String::from_utf8_lossy(stream).trim().to_string())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Windows searches the executable's own directory first, so a copy there makes
/// the binary self-contained.
fn place_dll_next_to_executable(dll: &Path, out: &Path) {
    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out; binaries land three
    // levels up, with examples and test binaries one level below that.
    let Some(target_dir) = out.ancestors().nth(3) else {
        return;
    };
    for dir in [target_dir.to_path_buf(), target_dir.join("deps")] {
        if dir.is_dir() {
            let dst = dir.join("cec.dll");
            let stale = match (
                dll.metadata().and_then(|m| m.modified()),
                dst.metadata().and_then(|m| m.modified()),
            ) {
                (Ok(s), Ok(d)) => s > d,
                _ => true,
            };
            if stale {
                let _ = std::fs::copy(dll, &dst);
            }
        }
    }
}
