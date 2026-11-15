use serde::{Deserialize, Serialize};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileFormat {
    Toml,
    Json,
    Yaml,
    Text,
}

impl FileFormat {
    pub fn from_path(path: &str) -> Self {
        let lower = path.trim().to_ascii_lowercase();
        if lower.ends_with(".toml") {
            Self::Toml
        } else if lower.ends_with(".json") {
            Self::Json
        } else if lower.ends_with(".yml") || lower.ends_with(".yaml") {
            Self::Yaml
        } else {
            Self::Text
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuralConflict {
    pub key_path: String,
    pub base: String,
    pub ours: String,
    pub theirs: String,
}

fn format_conflicts(format_label: &str, conflicts: &[StructuralConflict]) -> String {
    let mut out = format!("# Structural Merge Conflict ({format_label})\n");
    for c in conflicts {
        out.push_str(&format!(
            "<<<<<<< ours: {}\n{}\n||||||| base: {}\n{}\n=======\n{}\n>>>>>>> theirs: {}\n",
            c.key_path, c.ours, c.key_path, c.base, c.theirs, c.key_path
        ));
    }
    out
}

pub fn three_way_merge_by_path(
    target_path: &str,
    base: &str,
    ours: &str,
    theirs: &str,
) -> Result<String, String> {
    match FileFormat::from_path(target_path) {
        FileFormat::Toml => {
            if let Some(res) = try_merge_toml_structural(base, ours, theirs) {
                res
            } else {
                diffy::merge(base, ours, theirs)
            }
        }
        FileFormat::Json => {
            if let Some(res) = try_merge_json_structural(base, ours, theirs) {
                res
            } else {
                diffy::merge(base, ours, theirs)
            }
        }
        FileFormat::Yaml => {
            if let Some(res) = try_merge_yaml_structural(base, ours, theirs) {
                res
            } else {
                diffy::merge(base, ours, theirs)
            }
        }
        FileFormat::Text => diffy::merge(base, ours, theirs),
    }
}

pub fn merge_toml_structural(base: &str, ours: &str, theirs: &str) -> Result<String, String> {
    match try_merge_toml_structural(base, ours, theirs) {
        Some(result) => result,
        None => Err("at least one input document is not valid TOML".to_string()),
    }
}

fn try_merge_toml_structural(
    base: &str,
    ours: &str,
    theirs: &str,
) -> Option<Result<String, String>> {
    let base_doc = base.parse::<DocumentMut>().ok()?;
    let mut ours_doc = ours.parse::<DocumentMut>().ok()?;
    let theirs_doc = theirs.parse::<DocumentMut>().ok()?;

    let mut conflicts = Vec::new();
    merge_toml_tables(
        Some(base_doc.as_table()),
        ours_doc.as_table_mut(),
        theirs_doc.as_table(),
        "",
        &mut conflicts,
    );

    if conflicts.is_empty() {
        Some(Ok(ours_doc.to_string()))
    } else {
        Some(Err(format_conflicts("TOML", &conflicts)))
    }
}

fn collect_table_keys(base: Option<&Table>, ours: &Table, theirs: &Table) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(b) = base {
        for (k, item) in b.iter() {
            if !item.is_none() && !keys.iter().any(|existing| existing == k) {
                keys.push(k.to_string());
            }
        }
    }
    for (k, item) in ours.iter() {
        if !item.is_none() && !keys.iter().any(|existing| existing == k) {
            keys.push(k.to_string());
        }
    }
    for (k, item) in theirs.iter() {
        if !item.is_none() && !keys.iter().any(|existing| existing == k) {
            keys.push(k.to_string());
        }
    }
    keys
}

fn item_as_table_clone(item: &Item) -> Option<Table> {
    match item {
        Item::Table(t) => Some(t.clone()),
        Item::Value(Value::InlineTable(it)) => Some(it.clone().into_table()),
        _ => None,
    }
}

fn item_is_table_like(item: &Item) -> bool {
    matches!(item, Item::Table(_) | Item::Value(Value::InlineTable(_)))
}

fn merge_toml_tables(
    base: Option<&Table>,
    ours: &mut Table,
    theirs: &Table,
    prefix: &str,
    conflicts: &mut Vec<StructuralConflict>,
) {
    let keys = collect_table_keys(base, ours, theirs);

    for k in keys {
        let key_path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };

        let b_item = base.and_then(|t| t.get(&k)).filter(|i| !i.is_none());
        let o_item = ours.get(&k).filter(|i| !i.is_none()).cloned();
        let t_item = theirs.get(&k).filter(|i| !i.is_none());

        let both_table_like = match (o_item.as_ref(), t_item) {
            (Some(o), Some(t)) => {
                item_is_table_like(o)
                    && item_is_table_like(t)
                    && b_item.is_none_or(item_is_table_like)
            }
            _ => false,
        };

        if both_table_like {
            match ours.get_mut(&k) {
                Some(Item::Table(o_subtable)) => {
                    let b_sub = b_item.and_then(item_as_table_clone);
                    if let Some(t_sub) = t_item.and_then(item_as_table_clone) {
                        merge_toml_tables(b_sub.as_ref(), o_subtable, &t_sub, &key_path, conflicts);
                    }
                    continue;
                }
                Some(Item::Value(Value::InlineTable(o_inline))) => {
                    if let Some(Item::Value(Value::InlineTable(t_inline))) = t_item {
                        let b_inline = match b_item {
                            Some(Item::Value(Value::InlineTable(bi))) => Some(bi),
                            _ => None,
                        };
                        merge_toml_inline_tables(
                            b_inline, o_inline, t_inline, &key_path, conflicts,
                        );
                        continue;
                    } else if let Some(t_sub) = t_item.and_then(item_as_table_clone) {
                        let b_sub = b_item.and_then(item_as_table_clone);
                        let mut promoted = o_inline.clone().into_table();
                        merge_toml_tables(
                            b_sub.as_ref(),
                            &mut promoted,
                            &t_sub,
                            &key_path,
                            conflicts,
                        );
                        ours.insert(&k, Item::Table(promoted));
                        continue;
                    }
                }
                _ => {}
            }
        }

        let b_eq_t = opt_toml_items_equal(b_item, t_item);
        let b_eq_o = opt_toml_items_equal(b_item, o_item.as_ref());
        let o_eq_t = opt_toml_items_equal(o_item.as_ref(), t_item);

        if b_eq_t || o_eq_t {
            // Keep `ours`
        } else if b_eq_o {
            match t_item {
                Some(t_val) => {
                    apply_theirs_item_to_ours_table(ours, theirs, &k, t_val);
                }
                None => {
                    ours.remove(&k);
                }
            }
        } else {
            conflicts.push(StructuralConflict {
                key_path,
                base: format_opt_toml_item(b_item),
                ours: format_opt_toml_item(o_item.as_ref()),
                theirs: format_opt_toml_item(t_item),
            });
        }
    }
}

fn apply_theirs_item_to_ours_table(ours: &mut Table, theirs: &Table, key: &str, t_val: &Item) {
    if let (Some(Item::Value(existing_val)), Item::Value(new_val)) = (ours.get_mut(key), t_val) {
        let prefix = existing_val.decor().prefix().cloned();
        let suffix = existing_val.decor().suffix().cloned();
        let mut updated = new_val.clone();
        if let Some(p) = prefix {
            updated.decor_mut().set_prefix(p);
        }
        if let Some(s) = suffix {
            updated.decor_mut().set_suffix(s);
        }
        *existing_val = updated;
    } else if !ours.contains_key(key) {
        if let Some((theirs_key, _)) = theirs.get_key_value(key) {
            ours.insert_formatted(theirs_key, t_val.clone());
        } else {
            ours.insert(key, t_val.clone());
        }
    } else {
        ours.insert(key, t_val.clone());
    }
}

fn merge_toml_inline_tables(
    base: Option<&InlineTable>,
    ours: &mut InlineTable,
    theirs: &InlineTable,
    prefix: &str,
    conflicts: &mut Vec<StructuralConflict>,
) {
    let mut keys = Vec::new();
    if let Some(b) = base {
        for (k, _) in b.iter() {
            if !keys.iter().any(|existing| existing == k) {
                keys.push(k.to_string());
            }
        }
    }
    for (k, _) in ours.iter() {
        if !keys.iter().any(|existing| existing == k) {
            keys.push(k.to_string());
        }
    }
    for (k, _) in theirs.iter() {
        if !keys.iter().any(|existing| existing == k) {
            keys.push(k.to_string());
        }
    }

    for k in keys {
        let key_path = format!("{prefix}.{k}");
        let b_val = base.and_then(|t| t.get(&k));
        let o_val = ours.get(&k).cloned();
        let t_val = theirs.get(&k);

        if let (Some(Value::InlineTable(_)), Some(Value::InlineTable(t_sub))) =
            (o_val.as_ref(), t_val)
            && b_val.is_none_or(|v| matches!(v, Value::InlineTable(_)))
            && let Some(Value::InlineTable(o_sub)) = ours.get_mut(&k)
        {
            let b_sub = match b_val {
                Some(Value::InlineTable(bi)) => Some(bi),
                _ => None,
            };
            merge_toml_inline_tables(b_sub, o_sub, t_sub, &key_path, conflicts);
            continue;
        }

        let b_eq_t = opt_toml_values_equal(b_val, t_val);
        let b_eq_o = opt_toml_values_equal(b_val, o_val.as_ref());
        let o_eq_t = opt_toml_values_equal(o_val.as_ref(), t_val);

        if b_eq_t || o_eq_t {
            // Keep `ours`
        } else if b_eq_o {
            match t_val {
                Some(tv) => {
                    ours.insert(&k, tv.clone());
                }
                None => {
                    ours.remove(&k);
                }
            }
        } else {
            conflicts.push(StructuralConflict {
                key_path,
                base: b_val
                    .map(|v| v.to_string().trim().to_string())
                    .unwrap_or_else(|| "<none>".to_string()),
                ours: o_val
                    .as_ref()
                    .map(|v| v.to_string().trim().to_string())
                    .unwrap_or_else(|| "<deleted>".to_string()),
                theirs: t_val
                    .map(|v| v.to_string().trim().to_string())
                    .unwrap_or_else(|| "<deleted>".to_string()),
            });
        }
    }
}

fn opt_toml_items_equal(a: Option<&Item>, b: Option<&Item>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(ia), Some(ib)) => toml_items_equal(ia, ib),
        _ => false,
    }
}

fn toml_items_equal(a: &Item, b: &Item) -> bool {
    match (a, b) {
        (Item::None, Item::None) => true,
        (Item::Value(va), Item::Value(vb)) => toml_values_equal(va, vb),
        (Item::Table(ta), Item::Table(tb)) => toml_tables_equal(ta, tb),
        (Item::Table(ta), Item::Value(Value::InlineTable(ib)))
        | (Item::Value(Value::InlineTable(ib)), Item::Table(ta)) => {
            toml_tables_equal(ta, &ib.clone().into_table())
        }
        (Item::ArrayOfTables(aa), Item::ArrayOfTables(ab)) => toml_array_of_tables_equal(aa, ab),
        _ => false,
    }
}

fn opt_toml_values_equal(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(va), Some(vb)) => toml_values_equal(va, vb),
        _ => false,
    }
}

fn toml_values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(sa), Value::String(sb)) => sa.value() == sb.value(),
        (Value::Integer(ia), Value::Integer(ib)) => ia.value() == ib.value(),
        (Value::Float(fa), Value::Float(fb)) => fa.value().to_bits() == fb.value().to_bits(),
        (Value::Boolean(ba), Value::Boolean(bb)) => ba.value() == bb.value(),
        (Value::Datetime(da), Value::Datetime(db)) => da.value() == db.value(),
        (Value::Array(aa), Value::Array(ab)) => toml_arrays_equal(aa, ab),
        (Value::InlineTable(ta), Value::InlineTable(tb)) => toml_inline_tables_equal(ta, tb),
        _ => false,
    }
}

fn toml_arrays_equal(a: &Array, b: &Array) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(va, vb)| toml_values_equal(va, vb))
}

fn toml_inline_tables_equal(a: &InlineTable, b: &InlineTable) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .all(|(k, va)| b.get(k).is_some_and(|vb| toml_values_equal(va, vb)))
}

fn toml_tables_equal(a: &Table, b: &Table) -> bool {
    let a_keys: Vec<_> = a.iter().filter(|(_, i)| !i.is_none()).collect();
    let b_keys: Vec<_> = b.iter().filter(|(_, i)| !i.is_none()).collect();
    if a_keys.len() != b_keys.len() {
        return false;
    }
    a_keys.iter().all(|(k, ia)| {
        b.get(k)
            .filter(|ib| !ib.is_none())
            .is_some_and(|ib| toml_items_equal(ia, ib))
    })
}

fn toml_array_of_tables_equal(a: &ArrayOfTables, b: &ArrayOfTables) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(ta, tb)| toml_tables_equal(ta, tb))
}

fn format_opt_toml_item(item: Option<&Item>) -> String {
    match item {
        None | Some(Item::None) => "<none>".to_string(),
        Some(Item::Value(v)) => v.to_string().trim().to_string(),
        Some(Item::Table(t)) => t.to_string().trim().to_string(),
        Some(Item::ArrayOfTables(a)) => format!("[({} array of tables)]", a.len()),
    }
}

pub fn merge_json_structural(base: &str, ours: &str, theirs: &str) -> Result<String, String> {
    match try_merge_json_structural(base, ours, theirs) {
        Some(result) => result,
        None => Err("at least one input document is not valid JSON".to_string()),
    }
}

fn try_merge_json_structural(
    base: &str,
    ours: &str,
    theirs: &str,
) -> Option<Result<String, String>> {
    let base_val: serde_json::Value = serde_json::from_str(base).ok()?;
    let ours_val: serde_json::Value = serde_json::from_str(ours).ok()?;
    let theirs_val: serde_json::Value = serde_json::from_str(theirs).ok()?;

    let mut conflicts = Vec::new();
    let merged_val = merge_json_values(Some(&base_val), &ours_val, &theirs_val, "", &mut conflicts);

    if conflicts.is_empty() {
        let mut pretty = serde_json::to_string_pretty(&merged_val).ok()?;
        pretty.push('\n');
        Some(Ok(pretty))
    } else {
        Some(Err(format_conflicts("JSON", &conflicts)))
    }
}

fn merge_json_values(
    base: Option<&serde_json::Value>,
    ours: &serde_json::Value,
    theirs: &serde_json::Value,
    prefix: &str,
    conflicts: &mut Vec<StructuralConflict>,
) -> serde_json::Value {
    if let (serde_json::Value::Object(o_map), serde_json::Value::Object(t_map)) = (ours, theirs)
        && base.is_none_or(|b| b.is_object())
    {
        let b_map = base.and_then(|b| b.as_object());
        let mut keys = Vec::new();
        if let Some(bm) = b_map {
            for k in bm.keys() {
                if !keys.contains(k) {
                    keys.push(k.clone());
                }
            }
        }
        for k in o_map.keys() {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
        for k in t_map.keys() {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }

        let mut merged = serde_json::Map::new();
        for k in keys {
            let key_path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };

            let b_v = b_map.and_then(|m| m.get(&k));
            let o_v = o_map.get(&k);
            let t_v = t_map.get(&k);

            if let (Some(o_sub), Some(t_sub)) = (o_v, t_v)
                && o_sub.is_object()
                && t_sub.is_object()
                && b_v.is_none_or(|bv| bv.is_object())
            {
                let sub = merge_json_values(b_v, o_sub, t_sub, &key_path, conflicts);
                merged.insert(k, sub);
                continue;
            }

            if b_v == t_v {
                if let Some(ov) = o_v {
                    merged.insert(k, ov.clone());
                }
            } else if b_v == o_v {
                if let Some(tv) = t_v {
                    merged.insert(k, tv.clone());
                }
            } else if o_v == t_v {
                if let Some(ov) = o_v {
                    merged.insert(k, ov.clone());
                }
            } else {
                conflicts.push(StructuralConflict {
                    key_path,
                    base: format_opt_json(b_v),
                    ours: format_opt_json(o_v),
                    theirs: format_opt_json(t_v),
                });
            }
        }

        return serde_json::Value::Object(merged);
    }

    if base == Some(theirs) {
        ours.clone()
    } else if base == Some(ours) {
        theirs.clone()
    } else if ours == theirs {
        ours.clone()
    } else {
        let key_path = if prefix.is_empty() {
            "$".to_string()
        } else {
            prefix.to_string()
        };
        conflicts.push(StructuralConflict {
            key_path,
            base: format_opt_json(base),
            ours: format_opt_json(Some(ours)),
            theirs: format_opt_json(Some(theirs)),
        });
        ours.clone()
    }
}

fn format_opt_json(val: Option<&serde_json::Value>) -> String {
    match val {
        None => "<none>".to_string(),
        Some(v) => serde_json::to_string(v).unwrap_or_else(|_| v.to_string()),
    }
}

pub fn merge_yaml_structural(base: &str, ours: &str, theirs: &str) -> Result<String, String> {
    match try_merge_yaml_structural(base, ours, theirs) {
        Some(result) => result,
        None => Err("at least one input document is not valid block YAML".to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct YamlMap {
    entries: Vec<YamlEntry>,
    trailing_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct YamlEntry {
    leading_trivia: Vec<String>,
    key: String,
    header_line: String,
    body: YamlNode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum YamlNode {
    Scalar(String),
    Mapping(YamlMap),
    Sequence(Vec<YamlSeqItem>),
    RawBlock(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct YamlSeqItem {
    leading_trivia: Vec<String>,
    lines: Vec<String>,
    canonical: String,
}

fn try_merge_yaml_structural(
    base: &str,
    ours: &str,
    theirs: &str,
) -> Option<Result<String, String>> {
    let base_map = parse_yaml_map_root(base)?;
    let ours_map = parse_yaml_map_root(ours)?;
    let theirs_map = parse_yaml_map_root(theirs)?;

    let mut conflicts = Vec::new();
    let merged_map = merge_yaml_maps(Some(&base_map), &ours_map, &theirs_map, "", &mut conflicts);

    if conflicts.is_empty() {
        let mut rendered = String::new();
        emit_yaml_map(&merged_map, &mut rendered);
        if !rendered.ends_with('\n') && !rendered.is_empty() {
            rendered.push('\n');
        }
        Some(Ok(rendered))
    } else {
        Some(Err(format_conflicts("YAML", &conflicts)))
    }
}

fn parse_yaml_map_root(input: &str) -> Option<YamlMap> {
    let lines: Vec<String> = input.lines().map(str::to_string).collect();
    if lines.is_empty() {
        return None;
    }
    parse_yaml_map_slice(&lines, 0)
}

fn line_indent(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

fn is_trivia_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---"
}

fn split_yaml_key_value(trimmed: &str) -> Option<(&str, &str)> {
    if trimmed.starts_with("- ") || trimmed == "-" || trimmed.starts_with('#') {
        return None;
    }
    let colon_idx = trimmed.find(':')?;
    let key = trimmed[..colon_idx].trim();
    if key.is_empty() || key.contains(' ') || key.contains('\t') {
        return None;
    }
    let after = &trimmed[colon_idx + 1..];
    if !after.is_empty() && !after.starts_with(' ') && !after.starts_with('\t') {
        return None;
    }
    let val_no_comment = strip_yaml_inline_comment(after.trim());
    Some((key, val_no_comment))
}

fn strip_yaml_inline_comment(val: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = val.as_bytes();
    for i in 0..bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single && !in_double && (i == 0 || bytes[i - 1] == b' ') => {
                return val[..i].trim_end();
            }
            _ => {}
        }
    }
    val.trim()
}

fn parse_yaml_map_slice(lines: &[String], expected_indent: usize) -> Option<YamlMap> {
    let mut entries = Vec::new();
    let mut pending_trivia = Vec::new();
    let mut idx = 0;

    while idx < lines.len() {
        let line = &lines[idx];
        if is_trivia_line(line) {
            pending_trivia.push(line.clone());
            idx += 1;
            continue;
        }
        if line.starts_with('\t') {
            return None;
        }

        let indent = line_indent(line);
        if indent != expected_indent {
            return None;
        }

        let trimmed = &line[indent..];
        let (key, inline_val) = split_yaml_key_value(trimmed)?;
        let header_line = line.clone();
        let leading_trivia = std::mem::take(&mut pending_trivia);
        idx += 1;

        let child_start = idx;
        while idx < lines.len() {
            let next_line = &lines[idx];
            if is_trivia_line(next_line) {
                let mut lookahead = idx + 1;
                let mut has_deeper_child = false;
                while lookahead < lines.len() {
                    if !is_trivia_line(&lines[lookahead]) {
                        if line_indent(&lines[lookahead]) > expected_indent {
                            has_deeper_child = true;
                        }
                        break;
                    }
                    lookahead += 1;
                }
                if has_deeper_child {
                    idx += 1;
                    continue;
                } else {
                    break;
                }
            }
            if line_indent(next_line) > expected_indent {
                idx += 1;
            } else {
                break;
            }
        }

        let child_lines = &lines[child_start..idx];
        let body = if child_lines.is_empty() {
            YamlNode::Scalar(inline_val.to_string())
        } else if inline_val == "|" || inline_val == ">" || inline_val == "|-" || inline_val == ">-"
        {
            YamlNode::RawBlock(child_lines.to_vec())
        } else {
            let first_non_trivia = child_lines.iter().find(|l| !is_trivia_line(l))?;
            let child_indent = line_indent(first_non_trivia);
            let child_trimmed = &first_non_trivia[child_indent..];
            if child_trimmed.starts_with("- ") || child_trimmed == "-" {
                let seq = parse_yaml_sequence_slice(child_lines, child_indent)?;
                YamlNode::Sequence(seq)
            } else if let Some(sub_map) = parse_yaml_map_slice(child_lines, child_indent) {
                YamlNode::Mapping(sub_map)
            } else {
                YamlNode::RawBlock(child_lines.to_vec())
            }
        };

        entries.push(YamlEntry {
            leading_trivia,
            key: key.to_string(),
            header_line,
            body,
        });
    }

    if entries.is_empty() {
        return None;
    }

    Some(YamlMap {
        entries,
        trailing_lines: pending_trivia,
    })
}

fn parse_yaml_sequence_slice(lines: &[String], seq_indent: usize) -> Option<Vec<YamlSeqItem>> {
    let mut items = Vec::new();
    let mut pending_trivia = Vec::new();
    let mut idx = 0;

    while idx < lines.len() {
        let line = &lines[idx];
        if is_trivia_line(line) {
            pending_trivia.push(line.clone());
            idx += 1;
            continue;
        }
        let indent = line_indent(line);
        if indent != seq_indent {
            return None;
        }
        let trimmed = &line[indent..];
        if !trimmed.starts_with("- ") && trimmed != "-" {
            return None;
        }

        let leading_trivia = std::mem::take(&mut pending_trivia);
        let mut item_lines = vec![line.clone()];
        idx += 1;

        while idx < lines.len() {
            let next_line = &lines[idx];
            if is_trivia_line(next_line) {
                let mut lookahead = idx + 1;
                let mut continues_item = false;
                while lookahead < lines.len() {
                    if !is_trivia_line(&lines[lookahead]) {
                        if line_indent(&lines[lookahead]) > seq_indent {
                            continues_item = true;
                        }
                        break;
                    }
                    lookahead += 1;
                }
                if continues_item {
                    item_lines.push(next_line.clone());
                    idx += 1;
                    continue;
                } else {
                    break;
                }
            }
            if line_indent(next_line) > seq_indent {
                item_lines.push(next_line.clone());
                idx += 1;
            } else {
                break;
            }
        }

        let canonical = item_lines
            .iter()
            .filter(|l| !is_trivia_line(l))
            .map(|l| strip_yaml_inline_comment(l.trim()))
            .collect::<Vec<_>>()
            .join("\n");

        items.push(YamlSeqItem {
            leading_trivia,
            lines: item_lines,
            canonical,
        });
    }

    if items.is_empty() { None } else { Some(items) }
}

fn yaml_entry_semantically_equal(a: &YamlEntry, b: &YamlEntry) -> bool {
    if a.key != b.key {
        return false;
    }
    yaml_node_semantically_equal(&a.body, &b.body)
}

fn yaml_node_semantically_equal(a: &YamlNode, b: &YamlNode) -> bool {
    match (a, b) {
        (YamlNode::Scalar(sa), YamlNode::Scalar(sb)) => sa == sb,
        (YamlNode::Mapping(ma), YamlNode::Mapping(mb)) => yaml_maps_semantically_equal(ma, mb),
        (YamlNode::Sequence(sa), YamlNode::Sequence(sb)) => {
            sa.len() == sb.len()
                && sa
                    .iter()
                    .zip(sb.iter())
                    .all(|(ia, ib)| ia.canonical == ib.canonical)
        }
        (YamlNode::RawBlock(ra), YamlNode::RawBlock(rb)) => ra == rb,
        _ => false,
    }
}

fn yaml_maps_semantically_equal(a: &YamlMap, b: &YamlMap) -> bool {
    if a.entries.len() != b.entries.len() {
        return false;
    }
    a.entries.iter().all(|ea| {
        b.entries
            .iter()
            .find(|eb| eb.key == ea.key)
            .is_some_and(|eb| yaml_entry_semantically_equal(ea, eb))
    })
}

fn merge_yaml_maps(
    base: Option<&YamlMap>,
    ours: &YamlMap,
    theirs: &YamlMap,
    prefix: &str,
    conflicts: &mut Vec<StructuralConflict>,
) -> YamlMap {
    let mut keys = Vec::new();
    if let Some(b) = base {
        for e in &b.entries {
            if !keys.contains(&e.key) {
                keys.push(e.key.clone());
            }
        }
    }
    for e in &ours.entries {
        if !keys.contains(&e.key) {
            keys.push(e.key.clone());
        }
    }
    for e in &theirs.entries {
        if !keys.contains(&e.key) {
            keys.push(e.key.clone());
        }
    }

    let mut merged_entries = Vec::new();
    for k in keys {
        let key_path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };

        let b_e = base.and_then(|m| m.entries.iter().find(|e| e.key == k));
        let o_e = ours.entries.iter().find(|e| e.key == k);
        let t_e = theirs.entries.iter().find(|e| e.key == k);

        if let (Some(oe), Some(te)) = (o_e, t_e)
            && let (YamlNode::Mapping(o_sub), YamlNode::Mapping(t_sub)) = (&oe.body, &te.body)
            && b_e.is_none_or(|be| matches!(be.body, YamlNode::Mapping(_)))
        {
            let b_sub = match b_e.map(|be| &be.body) {
                Some(YamlNode::Mapping(bm)) => Some(bm),
                _ => None,
            };
            let merged_sub = merge_yaml_maps(b_sub, o_sub, t_sub, &key_path, conflicts);
            merged_entries.push(YamlEntry {
                leading_trivia: oe.leading_trivia.clone(),
                key: k,
                header_line: oe.header_line.clone(),
                body: YamlNode::Mapping(merged_sub),
            });
            continue;
        }

        if let (Some(oe), Some(te)) = (o_e, t_e)
            && let (YamlNode::Sequence(o_seq), YamlNode::Sequence(t_seq)) = (&oe.body, &te.body)
            && b_e.is_none_or(|be| matches!(be.body, YamlNode::Sequence(_)))
        {
            let b_seq = match b_e.map(|be| &be.body) {
                Some(YamlNode::Sequence(bs)) => Some(bs.as_slice()),
                _ => None,
            };
            let merged_seq = merge_yaml_sequences(b_seq, o_seq, t_seq);
            merged_entries.push(YamlEntry {
                leading_trivia: oe.leading_trivia.clone(),
                key: k,
                header_line: oe.header_line.clone(),
                body: YamlNode::Sequence(merged_seq),
            });
            continue;
        }

        let b_eq_t = opt_yaml_entries_equal(b_e, t_e);
        let b_eq_o = opt_yaml_entries_equal(b_e, o_e);
        let o_eq_t = opt_yaml_entries_equal(o_e, t_e);

        if b_eq_t {
            if let Some(oe) = o_e {
                merged_entries.push(oe.clone());
            }
        } else if b_eq_o {
            if let Some(te) = t_e {
                let mut adopted = te.clone();
                if let Some(oe) = o_e
                    && !oe.leading_trivia.is_empty()
                    && adopted.leading_trivia.is_empty()
                {
                    adopted.leading_trivia = oe.leading_trivia.clone();
                }
                merged_entries.push(adopted);
            }
        } else if o_eq_t {
            if let Some(oe) = o_e {
                merged_entries.push(oe.clone());
            }
        } else {
            conflicts.push(StructuralConflict {
                key_path,
                base: format_opt_yaml_entry(b_e),
                ours: format_opt_yaml_entry(o_e),
                theirs: format_opt_yaml_entry(t_e),
            });
        }
    }

    YamlMap {
        entries: merged_entries,
        trailing_lines: ours.trailing_lines.clone(),
    }
}

fn merge_yaml_sequences(
    base: Option<&[YamlSeqItem]>,
    ours: &[YamlSeqItem],
    theirs: &[YamlSeqItem],
) -> Vec<YamlSeqItem> {
    let base_set: Vec<&str> = base
        .unwrap_or(&[])
        .iter()
        .map(|i| i.canonical.as_str())
        .collect();
    let theirs_set: Vec<&str> = theirs.iter().map(|i| i.canonical.as_str()).collect();
    let ours_set: Vec<&str> = ours.iter().map(|i| i.canonical.as_str()).collect();

    let mut merged: Vec<YamlSeqItem> = Vec::new();
    for t_item in theirs {
        let was_in_base = base_set.contains(&t_item.canonical.as_str());
        let still_in_ours = ours_set.contains(&t_item.canonical.as_str());
        if was_in_base && !still_in_ours {
            continue;
        }
        if let Some(o_existing) = ours.iter().find(|o| o.canonical == t_item.canonical) {
            merged.push(o_existing.clone());
        } else {
            merged.push(t_item.clone());
        }
    }

    for o_item in ours {
        let was_in_base = base_set.contains(&o_item.canonical.as_str());
        let in_theirs = theirs_set.contains(&o_item.canonical.as_str());
        if !was_in_base && !in_theirs {
            merged.push(o_item.clone());
        }
    }

    merged
}

fn opt_yaml_entries_equal(a: Option<&YamlEntry>, b: Option<&YamlEntry>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(ea), Some(eb)) => yaml_entry_semantically_equal(ea, eb),
        _ => false,
    }
}

fn format_opt_yaml_entry(entry: Option<&YamlEntry>) -> String {
    match entry {
        None => "<none>".to_string(),
        Some(e) => match &e.body {
            YamlNode::Scalar(s) => s.clone(),
            _ => {
                let mut buf = String::new();
                emit_yaml_entry(e, &mut buf);
                buf.trim().to_string()
            }
        },
    }
}

fn emit_yaml_map(map: &YamlMap, out: &mut String) {
    for entry in &map.entries {
        emit_yaml_entry(entry, out);
    }
    for line in &map.trailing_lines {
        out.push_str(line);
        out.push('\n');
    }
}

fn emit_yaml_entry(entry: &YamlEntry, out: &mut String) {
    for t in &entry.leading_trivia {
        out.push_str(t);
        out.push('\n');
    }
    out.push_str(&entry.header_line);
    out.push('\n');
    match &entry.body {
        YamlNode::Scalar(_) => {}
        YamlNode::Mapping(sub) => emit_yaml_map(sub, out),
        YamlNode::Sequence(items) => {
            for item in items {
                for t in &item.leading_trivia {
                    out.push_str(t);
                    out.push('\n');
                }
                for l in &item.lines {
                    out.push_str(l);
                    out.push('\n');
                }
            }
        }
        YamlNode::RawBlock(lines) => {
            for l in lines {
                out.push_str(l);
                out.push('\n');
            }
        }
    }
}
