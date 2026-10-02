//! Translation applies only to presentation strings, never to user data.
//! The scoped context is used exclusively by the synchronous Rust tree builder.
use amikvm_core::preferences::Language;
use std::{borrow::Cow, cell::Cell, collections::BTreeMap, sync::OnceLock};

thread_local! {
    static LANGUAGE: Cell<Language> = const { Cell::new(Language::Chinese) };
}

pub fn current() -> Language {
    LANGUAGE.with(Cell::get)
}

pub fn scope<T>(language: Language, build: impl FnOnce() -> T) -> T {
    struct Restore(Language);
    impl Drop for Restore {
        fn drop(&mut self) {
            LANGUAGE.with(|active| active.set(self.0));
        }
    }
    let _restore = Restore(LANGUAGE.with(|active| active.replace(language)));
    build()
}

fn catalog() -> &'static BTreeMap<String, [String; 2]> {
    static CATALOG: OnceLock<BTreeMap<String, [String; 2]>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../locales/catalog.json"))
            .expect("translation catalog was checked during the build")
    })
}

pub fn text(source: &'static str) -> &'static str {
    text_in(current(), source)
}

pub fn text_in(language: Language, source: &'static str) -> &'static str {
    match language {
        Language::Chinese => source,
        Language::English => catalog().get(source).map_or(source, |entry| &entry[0]),
        Language::French => catalog().get(source).map_or(source, |entry| &entry[1]),
    }
}

/// A known backend status can be translated without touching embedded paths,
/// user names, protocol payloads or unknown diagnostic text.
pub fn message(source: &str) -> Cow<'_, str> {
    let translated = match current() {
        Language::Chinese => None,
        Language::English => catalog().get(source).map(|entry| entry[0].as_str()),
        Language::French => catalog().get(source).map(|entry| entry[1].as_str()),
    };
    Cow::Borrowed(translated.unwrap_or(source))
}

include!(concat!(env!("OUT_DIR"), "/locale_formats.rs"));

/// Search the displayed translations without moving filtering into React.
pub fn source_terms(query: &str) -> Vec<String> {
    if query.is_empty() {
        return vec![];
    }
    let query = query.to_lowercase();
    catalog()
        .iter()
        .filter(|(source, translations)| {
            source.to_lowercase().contains(&query)
                || translations
                    .iter()
                    .any(|text| text.to_lowercase().contains(&query))
        })
        .map(|(source, _)| source.clone())
        .collect()
}
