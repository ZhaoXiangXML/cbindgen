extern crate cbindgen;

use cbindgen::*;
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::{env, fs, str};

use pretty_assertions::assert_eq;

// Set automatically by cargo for integration tests
static CBINDGEN_PATH: &str = env!("CARGO_BIN_EXE_cbindgen");

fn style_str(style: Style) -> &'static str {
    match style {
        Style::Both => "both",
        Style::Tag => "tag",
        Style::Type => "type",
    }
}

fn run_cbindgen(
    path: &Path,
    output: Option<&Path>,
    language: Language,
    cpp_compat: bool,
    style: Option<Style>,
    generate_depfile: bool,
    package_version: bool,
) -> (Vec<u8>, Option<String>) {
    assert!(
        !(output.is_none() && generate_depfile),
        "generating a depfile requires outputting to a path"
    );
    let program = Path::new(CBINDGEN_PATH);
    let mut command = Command::new(program);
    if let Some(output) = output {
        command.arg("--output").arg(output);
    }
    let cbindgen_depfile = if generate_depfile {
        let depfile = tempfile::NamedTempFile::new().unwrap();
        command.arg("--depfile").arg(depfile.path());
        Some(depfile)
    } else {
        None
    };

    match language {
        Language::Cxx => {}
        Language::C => {
            command.arg("--lang").arg("c");

            if cpp_compat {
                command.arg("--cpp-compat");
            }
        }
        Language::Cython => {
            command.arg("--lang").arg("cython");
        }
        Language::CSharp => {
            command.arg("--lang").arg("csharp");
        }
    }

    if package_version {
        command.arg("--package-version");
    }

    if let Some(style) = style {
        command.arg("--style").arg(style_str(style));
    }

    let config = path.with_extension("toml");
    if config.exists() {
        command.arg("--config").arg(config);
    }

    command.arg(path);

    println!("Running: {:?}", command);
    let cbindgen_output = command.output().expect("failed to execute process");

    assert!(
        cbindgen_output.status.success(),
        "cbindgen failed: {:?} with error: {}",
        output,
        str::from_utf8(&cbindgen_output.stderr).unwrap_or_default()
    );

    let bindings = if let Some(output_path) = output {
        let mut bindings = Vec::new();
        // Ignore errors here, we have assertions on the expected output later.
        let _ = File::open(output_path).map(|mut file| {
            let _ = file.read_to_end(&mut bindings);
        });
        bindings
    } else {
        cbindgen_output.stdout
    };

    let depfile_contents = if let Some(mut depfile) = cbindgen_depfile {
        let mut raw = Vec::new();
        depfile.read_to_end(&mut raw).unwrap();
        Some(
            str::from_utf8(raw.as_slice())
                .expect("Invalid encoding encountered in depfile")
                .into(),
        )
    } else {
        None
    };
    (bindings, depfile_contents)
}

fn compile(
    cbindgen_output: &Path,
    tests_path: &Path,
    tmp_dir: &Path,
    language: Language,
    style: Option<Style>,
    skip_warning_as_error: bool,
) {
    let cc = match language {
        Language::Cxx => env::var("CXX").unwrap_or_else(|_| "g++".to_owned()),
        Language::C => env::var("CC").unwrap_or_else(|_| "gcc".to_owned()),
        Language::Cython => env::var("CYTHON").unwrap_or_else(|_| "cython".to_owned()),
        Language::CSharp => env::var("CSC").unwrap_or_else(|_| "csc".to_owned()),
    };

    let file_name = cbindgen_output
        .file_name()
        .expect("cbindgen output should be a file");
    let mut object = tmp_dir.join(file_name);
    object.set_extension("o");

    let mut command = Command::new(cc);
    match language {
        Language::Cxx | Language::C => {
            command.arg("-D").arg("DEFINED");
            command.arg("-I").arg(tests_path);
            command.arg("-Wall");
            if !skip_warning_as_error {
                command.arg("-Werror");
            }
            // `swift_name` is not recognzied by gcc.
            command.arg("-Wno-attributes");
            // clang warns about unused const variables.
            command.arg("-Wno-unused-const-variable");
            // clang also warns about returning non-instantiated templates (they could
            // be specialized, but they're not so it's fine).
            command.arg("-Wno-return-type-c-linkage");
            // deprecated warnings should not be errors as it's intended
            command.arg("-Wno-deprecated-declarations");

            if let Language::Cxx = language {
                // enum class is a c++11 extension which makes g++ on macos 10.14 error out
                // inline variables are are a c++17 extension
                command.arg("-std=c++17");
                // Prevents warnings when compiling .c files as c++.
                command.arg("-x").arg("c++");
                if let Ok(extra_flags) = env::var("CXXFLAGS") {
                    command.args(extra_flags.split_whitespace());
                }
            } else if let Ok(extra_flags) = env::var("CFLAGS") {
                command.args(extra_flags.split_whitespace());
            }

            if let Some(style) = style {
                command.arg("-D");
                command.arg(format!(
                    "CBINDGEN_STYLE_{}",
                    style_str(style).to_uppercase()
                ));
            }

            command.arg("-o").arg(&object);
            command.arg("-c").arg(cbindgen_output);
        }
        Language::Cython => {
            command.arg("-Wextra");
            if !skip_warning_as_error {
                // Our tests contain code that is deprecated in Cython 3.0.
                // Allowing warnings buys a little time.
                // command.arg("-Werror");
            }
            command.arg("-3");
            command.arg("-o").arg(&object);
            command.arg(cbindgen_output);
        }
        Language::CSharp => {}
    }

    println!("Running: {:?}", command);
    let out = command.output().expect("failed to compile");
    assert!(out.status.success(), "Output failed to compile: {:?}", out);

    if object.exists() {
        fs::remove_file(object).unwrap();
    }
}

const SKIP_WARNING_AS_ERROR_SUFFIX: &str = ".skip_warning_as_error";

#[allow(clippy::too_many_arguments)]
fn run_compile_test(
    name: &'static str,
    path: &Path,
    tmp_dir: &Path,
    language: Language,
    cpp_compat: bool,
    style: Option<Style>,
    cbindgen_outputs: &mut HashSet<Vec<u8>>,
    package_version: bool,
) {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_|{
        // When debugging in VSCode, the CARGO_MANIFEST_DIR is not set, fallback to current directory.
        // See https://github.com/rust-lang/rust-analyzer/issues/13022
        std::env::current_dir().unwrap().to_str().unwrap().to_owned()
    });
           
    let tests_path = Path::new(&crate_dir).join("tests");
    let mut generated_file = tests_path.join("expectations");
    fs::create_dir_all(&generated_file).unwrap();

    let style_ext = style
        // Cython is sensitive to dots, so we can't include any dots.
        .map(|style| match style {
            Style::Both => "_both",
            Style::Tag => "_tag",
            Style::Type => "",
        })
        .unwrap_or_default();
    let lang_ext = match language {
        Language::Cxx => ".cpp",
        Language::C if cpp_compat => ".compat.c",
        Language::C => ".c",
        // cbindgen is supposed to generate declaration files (`.pxd`), but `cython` compiler
        // is extension-sensitive and won't work on them, so we use implementation files (`.pyx`)
        // in the test suite.
        Language::Cython => ".pyx",
        Language::CSharp => ".cs",
    };

    let skip_warning_as_error = name.rfind(SKIP_WARNING_AS_ERROR_SUFFIX).is_some();

    let source_file =
        format!("{}{}{}", name, style_ext, lang_ext).replace(SKIP_WARNING_AS_ERROR_SUFFIX, "");

    generated_file.push(source_file);

    let (output_file, generate_depfile) = if env::var_os("CBINDGEN_TEST_VERIFY").is_some() {
        (None, false)
    } else {
        (
            Some(generated_file.as_path()),
            // --depfile does not work in combination with expanding yet, so we blacklist expanding tests.
            !(name.contains("expand") || name.contains("bitfield")),
        )
    };

    let (cbindgen_output, depfile_contents) = run_cbindgen(
        path,
        output_file,
        language,
        cpp_compat,
        style,
        generate_depfile,
        package_version,
    );
    // Skip depfile verification on Windows, as the paths are not normalized.
    #[cfg(not(windows))]
    if generate_depfile {
        let depfile = depfile_contents.expect("No depfile generated");
        assert!(!depfile.is_empty());
        let mut rules = depfile.split(':');
        let target = rules.next().expect("No target found");
        assert_eq!(target, generated_file.as_os_str().to_str().unwrap());
        let sources = rules.next().unwrap();
        // All the tests here only have one sourcefile.
        assert!(
            sources.contains(path.to_str().unwrap()),
            "Path: {:?}, Depfile contents: {}",
            path,
            depfile
        );
        assert_eq!(rules.count(), 0, "More than 1 rule in the depfile");
    }

    if cbindgen_outputs.contains(&cbindgen_output) {
        // We already generated an identical file previously.
        if env::var_os("CBINDGEN_TEST_VERIFY").is_some() {
            assert!(!generated_file.exists());
        } else if generated_file.exists() {
            fs::remove_file(&generated_file).unwrap();
        }
    } else {
        if env::var_os("CBINDGEN_TEST_VERIFY").is_some() {
            use std::str::from_utf8;
            let prev_cbindgen_output = fs::read(&generated_file).unwrap();
            let cbindgen_output = from_utf8(&cbindgen_output).unwrap();
            let prev_cbindgen_output = from_utf8(&prev_cbindgen_output).unwrap();
            assert_eq!(prev_cbindgen_output, cbindgen_output);
        } else {
            fs::write(&generated_file, &cbindgen_output).unwrap();
        }

        cbindgen_outputs.insert(cbindgen_output);

        if env::var_os("CBINDGEN_TEST_NO_COMPILE").is_some() {
            return;
        }

        compile(
            &generated_file,
            &tests_path,
            tmp_dir,
            language,
            style,
            skip_warning_as_error,
        );

        if language == Language::C && cpp_compat {
            compile(
                &generated_file,
                &tests_path,
                tmp_dir,
                Language::Cxx,
                style,
                skip_warning_as_error,
            );
        }
    }
}

fn test_file(name: &'static str, filename: &'static str) {
    let test = Path::new(filename);
    let tmp_dir = tempfile::Builder::new()
        .prefix("cbindgen-test-output")
        .tempdir()
        .expect("Creating tmp dir failed");
    let tmp_dir = tmp_dir.path();
    // Run tests in deduplication priority order. C++ compatibility tests are run first,
    // otherwise we would lose the C++ compiler run if they were deduplicated.
    let mut cbindgen_outputs = HashSet::new();
    for cpp_compat in &[true, false] {
        for style in &[Style::Type, Style::Tag, Style::Both] {
            run_compile_test(
                name,
                test,
                tmp_dir,
                Language::C,
                *cpp_compat,
                Some(*style),
                &mut cbindgen_outputs,
                false,
            );
        }
    }

    run_compile_test(
        name,
        test,
        tmp_dir,
        Language::Cxx,
        /* cpp_compat = */ false,
        None,
        &mut HashSet::new(),
        false,
    );

    // `Style::Both` should be identical to `Style::Tag` for Cython.
    let mut cbindgen_outputs = HashSet::new();
    for style in &[Style::Type, Style::Tag] {
        run_compile_test(
            name,
            test,
            tmp_dir,
            Language::Cython,
            /* cpp_compat = */ false,
            Some(*style),
            &mut cbindgen_outputs,
            false,
        );
    }
}

macro_rules! test_file {
    ($test_function_name:ident, $name:expr, $file:tt) => {
        #[test]
        fn $test_function_name() {
            test_file($name, $file);
        }
    };
}

// This file is generated by build.rs
include!(concat!(env!("OUT_DIR"), "/tests.rs"));
