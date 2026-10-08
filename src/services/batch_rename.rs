//! Batch rename rules: computing the new names of several files and
//! checking them, without any GTK (the dialog shows the result live, the
//! engine's `BatchRename` operation applies it).
//!
//! Rules change the name without its extension; the extension is kept.

use std::collections::{HashMap, HashSet};

use gettextrs::gettext;

use crate::services::file_ops::naming::{self, NameError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseChange {
    #[default]
    Lower,
    Upper,
    /// "Holiday In Rome".
    Title,
    /// "Holiday in rome".
    Sentence,
}

impl CaseChange {
    pub const ALL: [Self; 4] = [Self::Lower, Self::Upper, Self::Title, Self::Sentence];

    pub fn label(self) -> String {
        match self {
            Self::Lower => gettext("lowercase"),
            Self::Upper => gettext("UPPERCASE"),
            Self::Title => gettext("Title Case"),
            Self::Sentence => gettext("Sentence case"),
        }
    }

    fn apply(self, text: &str) -> String {
        match self {
            Self::Lower => text.to_lowercase(),
            Self::Upper => text.to_uppercase(),
            Self::Title => {
                let mut result = String::with_capacity(text.len());
                let mut word_start = true;
                for c in text.chars() {
                    if word_start {
                        result.extend(c.to_uppercase());
                    } else {
                        result.extend(c.to_lowercase());
                    }
                    word_start = c.is_whitespace() || matches!(c, '_' | '-' | '.' | '(' | '[');
                }
                result
            }
            Self::Sentence => {
                let mut chars = text.chars();
                match chars.next() {
                    Some(first) => first
                        .to_uppercase()
                        .chain(chars.flat_map(char::to_lowercase))
                        .collect(),
                    None => String::new(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// `{name}` is the current name, `{n}` a number counting from `start`
    /// (`{n:03}` pads it to 3 digits), `{date}` the date modified.
    Pattern {
        pattern: String,
        start: u32,
    },
    /// Replaces every occurrence of `find`.
    Replace {
        find: String,
        replace: String,
        match_case: bool,
    },
    Case(CaseChange),
}

/// A file to rename.
#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub is_dir: bool,
    /// Date modified as shown in `{date}` ("2026-10-08").
    pub date: String,
    /// Identifies the folder the file is in: names must be unique per folder.
    pub folder: String,
}

/// The new name of each source, in order.
pub fn new_names(rule: &Rule, sources: &[Source]) -> Vec<String> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let (stem, extension) = naming::split_extension(&source.name, source.is_dir);
            let stem = match rule {
                Rule::Pattern { pattern, start } => expand(
                    pattern,
                    stem,
                    start.saturating_add(index as u32),
                    &source.date,
                ),
                Rule::Replace {
                    find,
                    replace,
                    match_case,
                } => replace_all(stem, find, replace, *match_case),
                Rule::Case(change) => change.apply(stem),
            };
            format!("{stem}{extension}")
        })
        .collect()
}

/// Expands the tokens of a pattern; unknown ones are kept as typed.
fn expand(pattern: &str, name: &str, n: u32, date: &str) -> String {
    let mut result = String::with_capacity(pattern.len() + name.len());
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        result.push_str(&rest[..open]);
        let after = &rest[open..];
        let Some(close) = after.find('}') else {
            result.push_str(after);
            return result;
        };
        let token = &after[1..close];
        match token {
            "name" => result.push_str(name),
            "date" => result.push_str(date),
            "n" => result.push_str(&n.to_string()),
            _ => match token
                .strip_prefix("n:")
                .and_then(|width| width.parse::<usize>().ok())
            {
                Some(width) => result.push_str(&format!("{n:0width$}", width = width.min(10))),
                None => result.push_str(&after[..=close]),
            },
        }
        rest = &after[close + 1..];
    }
    result.push_str(rest);
    result
}

fn replace_all(text: &str, find: &str, replace: &str, match_case: bool) -> String {
    if find.is_empty() {
        return text.to_owned();
    }
    if match_case {
        return text.replace(find, replace);
    }
    // Case-insensitive, by characters (lowercasing can change byte lengths).
    let lower = |s: &str| {
        s.chars()
            .flat_map(char::to_lowercase)
            .collect::<Vec<char>>()
    };
    let needle = lower(find);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut result = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        // Match the needle against the characters from `i`, lowercased.
        let mut matched = 0;
        let mut consumed = 0;
        for &(_, c) in &chars[i..] {
            let lowered: Vec<char> = c.to_lowercase().collect();
            if needle.len() < matched + lowered.len()
                || needle[matched..matched + lowered.len()] != lowered[..]
            {
                break;
            }
            matched += lowered.len();
            consumed += 1;
            if matched == needle.len() {
                break;
            }
        }
        if matched == needle.len() && consumed > 0 {
            result.push_str(replace);
            i += consumed;
        } else {
            result.push(chars[i].1);
            i += 1;
        }
    }
    result
}

/// Why a new name can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    Invalid(NameError),
    /// Two files of the batch would get this name.
    Duplicate,
    /// Another file in the folder has this name.
    Exists,
}

impl Problem {
    pub fn message(&self) -> String {
        match self {
            Self::Invalid(err) => err.message(),
            Self::Duplicate => gettext("Another item would get the same name"),
            Self::Exists => gettext("An item with this name already exists"),
        }
    }
}

/// Checks the new names. `existing` holds the (folder, name) pairs of the
/// files not in the batch; names freed by the batch can be reused.
/// `portable`: the folder needs Windows-compatible names.
pub fn check(
    sources: &[Source],
    names: &[String],
    existing: &HashSet<(String, String)>,
    portable: bool,
) -> Vec<Option<Problem>> {
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    for (source, name) in sources.iter().zip(names) {
        *counts.entry((&source.folder, name)).or_default() += 1;
    }
    sources
        .iter()
        .zip(names)
        .map(|(source, name)| {
            if *name == source.name {
                return None;
            }
            if let Err(err) = naming::validate_name(name, portable) {
                return Some(Problem::Invalid(err));
            }
            if counts[&(source.folder.as_str(), name.as_str())] > 1 {
                return Some(Problem::Duplicate);
            }
            existing
                .contains(&(source.folder.clone(), name.clone()))
                .then_some(Problem::Exists)
        })
        .collect()
}
