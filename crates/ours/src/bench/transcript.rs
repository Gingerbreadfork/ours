//! What an agent did during a scenario, in the language-neutral form any
//! agent's harness can write (benchmark/FORMAT.md, "Transcripts").

use crate::bench::BenchError;
use crate::bench::scenario::json_files;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Transcript {
    /// The scenario's name.
    pub scenario: String,
    /// Who produced it, for the leaderboard.
    pub agent: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The latest turn posted before the agent did this.
    pub turn: usize,
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Whether nobody asked; worked out from the turn when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unprompted: Option<bool>,
    /// Where a message went; the turn's room when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    /// Whom a DM went to; the turn's speaker when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Seconds from the start; the turn's `t` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Message,
    Reaction,
    Dm,
    Action,
}

impl Entry {
    pub fn message(turn: usize, text: impl Into<String>) -> Entry {
        Entry {
            turn,
            kind: EntryKind::Message,
            text: Some(text.into()),
            emoji: None,
            action: None,
            unprompted: None,
            room: None,
            to: None,
            t: None,
        }
    }

    pub fn dm(turn: usize, to: impl Into<String>, text: impl Into<String>) -> Entry {
        Entry {
            kind: EntryKind::Dm,
            to: Some(to.into()),
            ..Entry::message(turn, text)
        }
    }

    pub fn reaction(turn: usize, emoji: impl Into<String>) -> Entry {
        Entry {
            kind: EntryKind::Reaction,
            text: None,
            emoji: Some(emoji.into()),
            ..Entry::message(turn, "")
        }
    }

    pub fn action(turn: usize, action: impl Into<String>) -> Entry {
        Entry {
            kind: EntryKind::Action,
            text: None,
            action: Some(action.into()),
            ..Entry::message(turn, "")
        }
    }

    pub fn unprompted(mut self, unprompted: bool) -> Entry {
        self.unprompted = Some(unprompted);
        self
    }
}

// A file holds either the full object or only the list of entries.
#[derive(Deserialize)]
#[serde(untagged)]
enum Wire {
    Full {
        #[serde(default)]
        scenario: String,
        #[serde(default)]
        agent: String,
        entries: Vec<Entry>,
    },
    Bare(Vec<Entry>),
}

impl<'de> Deserialize<'de> for Transcript {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Full {
                scenario,
                agent,
                entries,
            } => Transcript {
                scenario,
                agent,
                entries,
            },
            Wire::Bare(entries) => Transcript {
                entries,
                ..Transcript::default()
            },
        })
    }
}

/// Reads one transcript file. A bare list takes its scenario name from the
/// file name.
pub fn load_transcript(path: &Path) -> Result<Transcript, BenchError> {
    let text = std::fs::read_to_string(path).map_err(|source| BenchError::Read {
        path: path.to_owned(),
        source,
    })?;
    let mut transcript: Transcript =
        serde_json::from_str(&text).map_err(|source| BenchError::Parse {
            path: path.to_owned(),
            what: "transcript",
            source,
        })?;
    if transcript.scenario.is_empty()
        && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
    {
        transcript.scenario = stem.to_owned();
    }
    Ok(transcript)
}

/// A file, or every `*.json` transcript in a directory, by scenario name.
pub fn load_transcripts(path: &Path) -> Result<BTreeMap<String, Transcript>, BenchError> {
    let paths = if path.is_dir() {
        json_files(path)?
    } else {
        vec![path.to_owned()]
    };
    let mut out = BTreeMap::new();
    for p in paths {
        let t = load_transcript(&p)?;
        out.insert(t.scenario.clone(), t);
    }
    Ok(out)
}

/// Writes a transcript as pretty JSON.
pub fn save_transcript(path: &Path, transcript: &Transcript) -> Result<(), BenchError> {
    let write = |source| BenchError::Write {
        path: path.to_owned(),
        source,
    };
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(write)?;
    }
    let text = serde_json::to_string_pretty(transcript).map_err(|e| BenchError::Write {
        path: path.to_owned(),
        source: std::io::Error::other(e),
    })?;
    std::fs::write(path, text + "\n").map_err(write)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_list_and_a_full_object_both_load() {
        let bare: Transcript = serde_json::from_str(
            r#"[{ "turn": 2, "kind": "message", "text": "hi" },
                { "turn": 3, "kind": "action", "action": "open_poll", "unprompted": true }]"#,
        )
        .unwrap();
        assert_eq!(bare.entries.len(), 2);
        assert_eq!(bare.entries[1].unprompted, Some(true));
        let full: Transcript =
            serde_json::from_str(r#"{ "scenario": "football", "agent": "bot", "entries": [] }"#)
                .unwrap();
        assert_eq!(full.scenario, "football");
        let back: Transcript = serde_json::from_str(
            &serde_json::to_string(&Transcript {
                entries: bare.entries.clone(),
                ..full
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(back.entries, bare.entries);
    }
}
