use lime_protocol::DictionaryEntry;

/// Name of the mutable Rime-Ice user dictionary database.
pub(super) const RIME_ICE_USER_DICT: &str = "rime_ice";

pub(super) fn parse_user_dictionary(text: &str) -> Result<Vec<DictionaryEntry>, String> {
    let mut entries = Vec::new();
    for (line_number, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(format!(
                "Rime user dictionary line {} has {} tab-separated fields; expected 3",
                line_number + 1,
                fields.len()
            ));
        }
        let value = fields[0];
        let pinyin = fields[1].trim();
        let weight = fields[2].trim().parse().map_err(|_| {
            format!(
                "Rime user dictionary line {} has an invalid weight",
                line_number + 1
            )
        })?;
        if value.trim().is_empty() || pinyin.is_empty() {
            return Err(format!(
                "Rime user dictionary line {} contains an empty field",
                line_number + 1
            ));
        }
        entries.push(DictionaryEntry {
            pinyin: pinyin.to_owned(),
            text: value.to_owned(),
            weight,
        });
    }
    Ok(entries)
}
