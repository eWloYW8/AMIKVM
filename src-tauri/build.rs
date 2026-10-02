fn main() {
    println!(
        "cargo:rustc-env=AMIKVM_BUILD_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    let rustc = std::process::Command::new(std::env::var_os("RUSTC").unwrap())
        .arg("--version")
        .output()
        .unwrap();
    assert!(rustc.status.success());
    println!(
        "cargo:rustc-env=AMIKVM_RUSTC={}",
        String::from_utf8(rustc.stdout).unwrap().trim()
    );
    println!("cargo:rerun-if-changed=../Cargo.lock");
    let lock = std::fs::read_to_string("../Cargo.lock").unwrap();
    for (package, key) in [
        ("tauri", "TAURI"),
        ("openh264", "OPENH264"),
        ("mp4", "MP4"),
        ("fatfs", "FATFS"),
    ] {
        let block = lock
            .split("[[package]]")
            .find(|block| {
                block
                    .lines()
                    .any(|line| line == format!("name = \"{package}\""))
            })
            .unwrap();
        let version = block
            .lines()
            .find_map(|line| line.strip_prefix("version = "))
            .unwrap();
        let version: String = serde_json::from_str(version).unwrap();
        println!("cargo:rustc-env=AMIKVM_{key}_VERSION={version}");
    }
    println!("cargo:rerun-if-changed=locales/catalog.json");
    let catalog: std::collections::BTreeMap<String, [String; 2]> =
        serde_json::from_slice(&std::fs::read("locales/catalog.json").unwrap()).unwrap();
    validate_ui(&catalog);
    let mut macros = String::from("macro_rules! lformat {\n");
    for (source, translations) in catalog {
        let placeholders = placeholders(&source);
        for translation in &translations {
            assert!(!translation.is_empty(), "empty translation for {source}");
            assert_eq!(
                signature(&placeholders),
                signature(&self::placeholders(translation)),
                "changed format arguments in {source}: {translation}"
            );
        }
        if placeholders.is_empty() {
            continue;
        }
        let quote = |text: &str| serde_json::to_string(text).unwrap();
        macros.push_str(&format!(
            "({} $(, $($args:tt)*)?) => {{{{ match $crate::locale::current() {{\n\
             amikvm_core::preferences::Language::Chinese => format!({} $(, $($args)*)?),\n\
             amikvm_core::preferences::Language::English => format!({} $(, $($args)*)?),\n\
             amikvm_core::preferences::Language::French => format!({} $(, $($args)*)?),\n\
             }} }}}};\n",
            quote(&source),
            quote(&source),
            quote(&translations[0]),
            quote(&translations[1]),
        ));
    }
    macros.push_str("}\n");
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(output.join("locale_formats.rs"), macros).unwrap();
    tauri_build::build()
}

fn validate_ui(catalog: &std::collections::BTreeMap<String, [String; 2]>) {
    println!("cargo:rerun-if-changed=src/ui/view.rs");
    let source = std::fs::read_to_string("src/ui/view.rs").unwrap();
    let mut characters = source.char_indices();
    while let Some((start, character)) = characters.next() {
        if character != '"' {
            continue;
        }
        let end = loop {
            let (offset, character) = characters.next().expect("unclosed UI string");
            if character == '\\' {
                characters.next();
            } else if character == '"' {
                break offset;
            }
        };
        let literal: String = serde_json::from_str(&source[start..=end]).unwrap();
        if literal.contains(|character| ('\u{3400}'..='\u{9fff}').contains(&character)) {
            assert!(
                literal == "简体中文" || catalog.contains_key(&literal),
                "missing interface translation: {literal}"
            );
        }
    }
}

fn signature(arguments: &[String]) -> (Vec<&str>, Vec<&str>) {
    let (mut named, positional): (Vec<_>, Vec<_>) =
        arguments.iter().map(String::as_str).partition(|argument| {
            argument
                .starts_with(|character: char| character.is_ascii_alphabetic() || character == '_')
        });
    named.sort_unstable();
    (positional, named)
}

fn placeholders(source: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '{' {
            if characters.peek() == Some(&'{') {
                characters.next();
                continue;
            }
            let mut argument = String::new();
            loop {
                let character = characters.next().expect("unclosed format argument");
                if character == '}' {
                    break;
                }
                assert_ne!(character, '{', "nested format argument");
                argument.push(character);
            }
            arguments.push(argument);
        } else if character == '}' {
            assert_eq!(characters.next(), Some('}'), "unmatched closing brace");
        }
    }
    arguments
}
