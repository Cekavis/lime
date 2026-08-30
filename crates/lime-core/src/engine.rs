use crate::CoreError;
use lime_protocol::{Candidate, DictionaryEntry, ErrorCode};
use std::{collections::BTreeMap, fs, path::Path};

pub trait CandidateEngine: Send {
    fn candidates(&mut self, preedit: &str) -> Result<Vec<Candidate>, CoreError>;
    fn learn(&mut self, pinyin: &str, text: &str) -> Result<(), CoreError>;
    fn export_dictionary(&self) -> Vec<DictionaryEntry>;
    fn import_dictionary(&mut self, entries: &[DictionaryEntry]) -> Result<(), CoreError>;
    fn clear_dictionary(&mut self);
}

#[derive(Clone, Debug)]
pub struct RimeEngine {
    dictionary: BTreeMap<String, Vec<DictionaryEntry>>,
}

impl Default for RimeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RimeEngine {
    pub fn new() -> Self {
        let mut engine = Self {
            dictionary: BTreeMap::new(),
        };
        engine
            .import_dictionary(&Self::builtin_entries())
            .expect("built-in dictionary is valid");
        engine
    }

    fn builtin_entries() -> Vec<DictionaryEntry> {
        let entries = [
            ("nihao", "你好"),
            ("ninhao", "您好"),
            ("zhongguo", "中国"),
            ("beijing", "北京"),
            ("shanghai", "上海"),
            ("xiexie", "谢谢"),
            ("zaijian", "再见"),
            ("wo", "我"),
            ("ni", "你"),
            ("hao", "好"),
            ("de", "的"),
            ("shi", "是"),
            ("ren", "人"),
            ("zhong", "中"),
        ];
        entries
            .into_iter()
            .map(|(pinyin, text)| DictionaryEntry {
                pinyin: pinyin.to_owned(),
                text: text.to_owned(),
                weight: 0,
            })
            .collect()
    }

    pub fn from_dictionary_file(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let mut engine = Self::new();
        let text = fs::read_to_string(path)
            .map_err(|e| CoreError::new(ErrorCode::RimeInitializationFailed, e.to_string()))?;
        let entries = text.lines().filter_map(parse_entry).collect::<Vec<_>>();
        engine.import_dictionary(&entries)?;
        Ok(engine)
    }

    fn normalized(input: &str) -> String {
        input
            .chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect()
    }
}

impl CandidateEngine for RimeEngine {
    fn candidates(&mut self, preedit: &str) -> Result<Vec<Candidate>, CoreError> {
        let key = Self::normalized(preedit);
        if key.is_empty() {
            return Ok(Vec::new());
        }
        let syllables = preedit.split_whitespace().count().max(1);
        let mut values = self.dictionary.get(&key).cloned().unwrap_or_default();
        if values.is_empty() {
            for (pinyin, entries) in &self.dictionary {
                if pinyin.starts_with(&key) {
                    values.extend(entries.iter().cloned());
                }
            }
        }
        values.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.text.cmp(&b.text)));
        values.dedup_by(|a, b| a.text == b.text);
        Ok(values
            .into_iter()
            .filter(|entry| entry.text.chars().count() >= syllables)
            .map(|entry| Candidate {
                display_text: entry.text.clone(),
                commit_text: entry.text,
            })
            .collect())
    }

    fn learn(&mut self, pinyin: &str, text: &str) -> Result<(), CoreError> {
        let pinyin = Self::normalized(pinyin);
        if pinyin.is_empty() || text.trim().is_empty() {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "pinyin and text must not be empty",
            ));
        }
        let entries = self.dictionary.entry(pinyin.clone()).or_default();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.text == text) {
            entry.weight = entry.weight.saturating_add(1);
        } else {
            entries.push(DictionaryEntry {
                pinyin,
                text: text.to_owned(),
                weight: 1,
            });
        }
        Ok(())
    }

    fn export_dictionary(&self) -> Vec<DictionaryEntry> {
        self.dictionary
            .values()
            .flat_map(|items| items.iter().cloned())
            .collect()
    }

    fn import_dictionary(&mut self, entries: &[DictionaryEntry]) -> Result<(), CoreError> {
        for entry in entries {
            if Self::normalized(&entry.pinyin).is_empty() || entry.text.trim().is_empty() {
                return Err(CoreError::new(
                    ErrorCode::InvalidRequest,
                    "dictionary entry contains empty fields",
                ));
            }
        }
        for entry in entries {
            let key = Self::normalized(&entry.pinyin);
            let bucket = self.dictionary.entry(key.clone()).or_default();
            if let Some(existing) = bucket.iter_mut().find(|item| item.text == entry.text) {
                existing.weight = entry.weight;
            } else {
                bucket.push(DictionaryEntry {
                    pinyin: key,
                    text: entry.text.clone(),
                    weight: entry.weight,
                });
            }
        }
        Ok(())
    }
    fn clear_dictionary(&mut self) {
        self.dictionary.clear();
        let _ = self.import_dictionary(&Self::builtin_entries());
    }
}

fn parse_entry(line: &str) -> Option<DictionaryEntry> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut fields = line.split_whitespace();
    Some(DictionaryEntry {
        text: fields.next()?.to_owned(),
        pinyin: fields.next()?.to_owned(),
        weight: fields.next().and_then(|v| v.parse().ok()).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lookup_and_learning_are_deterministic() {
        let mut engine = RimeEngine::new();
        assert_eq!(engine.candidates("nihao").unwrap()[0].commit_text, "你好");
        engine.learn("nihao", "拟好").unwrap();
        assert!(engine
            .candidates("nihao")
            .unwrap()
            .iter()
            .any(|c| c.commit_text == "拟好"));
    }
}
