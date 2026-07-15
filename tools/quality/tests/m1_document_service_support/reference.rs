use std::collections::{BTreeMap, BTreeSet};

use super::{
    CaseContract, OutlineItemSummary, OutlineSummary, PageSummary, ServiceOutcome, ServiceResult,
};

const REFERENCE_PARSE_ERROR: &str = "RPE-REFERENCE-0001";
const REFERENCE_SEMANTIC_ERROR: &str = "RPE-REFERENCE-0002";
const PAGE_COUNT_MISMATCH: &str = "RPE-DOCUMENT-0033";
const OUTLINE_PREV_MISMATCH: &str = "RPE-DOCUMENT-0041";

type Dictionary = BTreeMap<String, ReferenceValue>;

#[derive(Clone, Debug, Eq, PartialEq)]
enum ReferenceValue {
    Null,
    Name(String),
    Integer(i64),
    Reference(u32),
    Array(Vec<ReferenceValue>),
    String(String),
    Dictionary(Dictionary),
}

struct ReferenceDocument {
    root: u32,
    objects: BTreeMap<u32, Dictionary>,
}

/// Independently evaluates the bounded service subset directly from source bytes.
///
/// This parser deliberately shares no syntax, xref, object, document, or session code with the
/// Native target. Its accepted language is the small self-authored direct-dictionary profile used
/// by the content-addressed M1 service cases.
pub fn reference_result(bytes: &[u8], contract: &CaseContract) -> ServiceResult {
    let document = match ReferenceDocument::parse(bytes, contract.max_objects) {
        Ok(document) => document,
        Err(error) => {
            return ServiceResult {
                page_count: ServiceOutcome::Failed(error),
                outline: ServiceOutcome::Failed(error),
            };
        }
    };
    let catalog = match document.objects.get(&document.root) {
        Some(catalog) if name(catalog, "Type") == Some("Catalog") => catalog,
        _ => {
            return ServiceResult {
                page_count: ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR),
                outline: ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR),
            };
        }
    };
    ServiceResult {
        page_count: evaluate_page_count(&document, catalog, contract.max_pages),
        outline: evaluate_outline(&document, catalog, contract.max_outline_items),
    }
}

impl ReferenceDocument {
    fn parse(bytes: &[u8], max_objects: u64) -> Result<Self, &'static str> {
        let input = std::str::from_utf8(bytes).map_err(|_| REFERENCE_PARSE_ERROR)?;
        let mut objects = BTreeMap::new();
        let mut lines = input.lines();
        while let Some(line) = lines.next() {
            let fields: Vec<_> = line.split_ascii_whitespace().collect();
            if fields.len() != 3 || fields[2] != "obj" {
                continue;
            }
            let number = fields[0]
                .parse::<u32>()
                .map_err(|_| REFERENCE_PARSE_ERROR)?;
            if number == 0 || fields[1] != "0" {
                return Err(REFERENCE_PARSE_ERROR);
            }
            let mut body = String::new();
            loop {
                let object_line = lines.next().ok_or(REFERENCE_PARSE_ERROR)?;
                if object_line == "endobj" {
                    break;
                }
                if !body.is_empty() {
                    body.push(' ');
                }
                body.push_str(object_line);
            }
            let dictionary = ValueParser::new(body.as_bytes()).parse_complete_dictionary()?;
            if objects.insert(number, dictionary).is_some()
                || u64::try_from(objects.len()).unwrap_or(u64::MAX) > max_objects
            {
                return Err(REFERENCE_PARSE_ERROR);
            }
        }
        if objects.is_empty() {
            return Err(REFERENCE_PARSE_ERROR);
        }
        let trailer = input.rfind("trailer\n").ok_or(REFERENCE_PARSE_ERROR)?;
        let trailer = &input[trailer + "trailer\n".len()..];
        let trailer = ValueParser::new(trailer.as_bytes()).parse_dictionary()?;
        let root = required_reference(&trailer, "Root").ok_or(REFERENCE_PARSE_ERROR)?;
        Ok(Self { root, objects })
    }
}

fn evaluate_page_count(
    document: &ReferenceDocument,
    catalog: &Dictionary,
    max_pages: u64,
) -> ServiceOutcome<PageSummary> {
    let Some(root) = required_reference(catalog, "Pages") else {
        return ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR);
    };
    let mut active = BTreeSet::new();
    let mut complete = BTreeSet::new();
    match count_page_node(document, root, None, max_pages, &mut active, &mut complete) {
        Ok(page_count) => ServiceOutcome::Ready(PageSummary { page_count }),
        Err(error) => ServiceOutcome::Failed(error),
    }
}

fn count_page_node(
    document: &ReferenceDocument,
    node: u32,
    expected_parent: Option<u32>,
    max_pages: u64,
    active: &mut BTreeSet<u32>,
    complete: &mut BTreeSet<u32>,
) -> Result<u64, &'static str> {
    if complete.contains(&node) || !active.insert(node) {
        return Err(REFERENCE_SEMANTIC_ERROR);
    }
    let dictionary = document
        .objects
        .get(&node)
        .ok_or(REFERENCE_SEMANTIC_ERROR)?;
    if optional_reference(dictionary, "Parent")? != expected_parent {
        return Err(REFERENCE_SEMANTIC_ERROR);
    }
    let page_count = match name(dictionary, "Type") {
        Some("Page") => 1,
        Some("Pages") => {
            let children = references(dictionary, "Kids").ok_or(REFERENCE_SEMANTIC_ERROR)?;
            if children.is_empty() {
                return Err(REFERENCE_SEMANTIC_ERROR);
            }
            let mut total = 0_u64;
            for child in children {
                total = total
                    .checked_add(count_page_node(
                        document,
                        child,
                        Some(node),
                        max_pages,
                        active,
                        complete,
                    )?)
                    .ok_or(REFERENCE_SEMANTIC_ERROR)?;
                if total > max_pages {
                    return Err(REFERENCE_SEMANTIC_ERROR);
                }
            }
            if integer(dictionary, "Count").and_then(|value| u64::try_from(value).ok())
                != Some(total)
            {
                return Err(PAGE_COUNT_MISMATCH);
            }
            total
        }
        _ => return Err(REFERENCE_SEMANTIC_ERROR),
    };
    active.remove(&node);
    complete.insert(node);
    Ok(page_count)
}

fn evaluate_outline(
    document: &ReferenceDocument,
    catalog: &Dictionary,
    max_items: u64,
) -> ServiceOutcome<OutlineSummary> {
    let root_reference = match optional_reference(catalog, "Outlines") {
        Ok(Some(reference)) => reference,
        Ok(None) => {
            return ServiceOutcome::Ready(OutlineSummary {
                root_object_number: None,
                root_count: None,
                visible_items: 0,
                items: Vec::new(),
            });
        }
        Err(error) => return ServiceOutcome::Failed(error),
    };
    let Some(root) = document.objects.get(&root_reference) else {
        return ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR);
    };
    match optional_name(root, "Type") {
        Ok(None | Some("Outlines")) => {}
        Ok(Some(_)) | Err(_) => return ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR),
    }
    let root_count = match optional_integer(root, "Count") {
        Ok(count) => count,
        Err(error) => return ServiceOutcome::Failed(error),
    };
    if root_count.is_some_and(|count| count < 0) {
        return ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR);
    }
    let (first, last) = match paired_references(root, "First", "Last") {
        Ok(pair) => pair,
        Err(error) => return ServiceOutcome::Failed(error),
    };
    let Some((first, last)) = first.zip(last) else {
        if root_count.is_some() {
            return ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR);
        }
        return ServiceOutcome::Ready(OutlineSummary {
            root_object_number: Some(root_reference),
            root_count: None,
            visible_items: 0,
            items: Vec::new(),
        });
    };
    let mut items = Vec::new();
    let mut visited = BTreeSet::new();
    let stats = match walk_outline_chain(
        document,
        root_reference,
        first,
        last,
        None,
        1,
        max_items,
        &mut items,
        &mut visited,
    ) {
        Ok(stats) => stats,
        Err(error) => return ServiceOutcome::Failed(error),
    };
    let Some(root_count) = root_count.and_then(|value| u64::try_from(value).ok()) else {
        return ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR);
    };
    if root_count != stats.visible_items {
        return ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR);
    }
    ServiceOutcome::Ready(OutlineSummary {
        root_object_number: Some(root_reference),
        root_count: Some(root_count),
        visible_items: stats.visible_items,
        items,
    })
}

#[derive(Clone, Copy)]
struct ChainStats {
    direct_items: u64,
    total_items: u64,
    visible_items: u64,
}

#[allow(clippy::too_many_arguments)]
fn walk_outline_chain(
    document: &ReferenceDocument,
    parent_reference: u32,
    first: u32,
    last: u32,
    parent_index: Option<usize>,
    depth: u64,
    max_items: u64,
    items: &mut Vec<OutlineItemSummary>,
    visited: &mut BTreeSet<u32>,
) -> Result<ChainStats, &'static str> {
    let mut current = first;
    let mut previous = None;
    let mut direct_items = 0_u64;
    let mut total_items = 0_u64;
    let mut visible_items = 0_u64;
    loop {
        if !visited.insert(current) || u64::try_from(visited.len()).unwrap_or(u64::MAX) > max_items
        {
            return Err(REFERENCE_SEMANTIC_ERROR);
        }
        let dictionary = document
            .objects
            .get(&current)
            .ok_or(REFERENCE_SEMANTIC_ERROR)?;
        if required_reference(dictionary, "Parent") != Some(parent_reference) {
            return Err(REFERENCE_SEMANTIC_ERROR);
        }
        if optional_reference(dictionary, "Prev")? != previous {
            return Err(OUTLINE_PREV_MISMATCH);
        }
        let title = string(dictionary, "Title")
            .ok_or(REFERENCE_SEMANTIC_ERROR)?
            .to_owned();
        let declared_count = optional_integer(dictionary, "Count")?;
        let target_kind = match (
            optional_destination(dictionary, "Dest")?,
            optional_action(dictionary, "A")?,
        ) {
            (false, false) => "none",
            (true, false) => "destination",
            (false, true) => "action",
            (true, true) => return Err(REFERENCE_SEMANTIC_ERROR),
        };
        let item_index = items.len();
        items.push(OutlineItemSummary {
            object_number: current,
            parent_index,
            depth,
            title,
            declared_count,
            target_kind,
            direct_children: 0,
            visible_descendants_if_open: 0,
        });
        let (child_first, child_last) = paired_references(dictionary, "First", "Last")?;
        let child_stats = if let Some((child_first, child_last)) = child_first.zip(child_last) {
            walk_outline_chain(
                document,
                current,
                child_first,
                child_last,
                Some(item_index),
                depth.checked_add(1).ok_or(REFERENCE_SEMANTIC_ERROR)?,
                max_items,
                items,
                visited,
            )?
        } else {
            ChainStats {
                direct_items: 0,
                total_items: 0,
                visible_items: 0,
            }
        };
        if declared_count
            .map(i64::unsigned_abs)
            .unwrap_or(child_stats.visible_items)
            != child_stats.visible_items
            || (declared_count.is_none() && child_stats.total_items != 0)
        {
            return Err(REFERENCE_SEMANTIC_ERROR);
        }
        items[item_index].direct_children = child_stats.direct_items;
        items[item_index].visible_descendants_if_open = child_stats.visible_items;
        direct_items = direct_items
            .checked_add(1)
            .ok_or(REFERENCE_SEMANTIC_ERROR)?;
        total_items = total_items
            .checked_add(1)
            .and_then(|value| value.checked_add(child_stats.total_items))
            .ok_or(REFERENCE_SEMANTIC_ERROR)?;
        visible_items = visible_items
            .checked_add(1)
            .and_then(|value| {
                if declared_count.is_some_and(|count| count > 0) {
                    value.checked_add(child_stats.visible_items)
                } else {
                    Some(value)
                }
            })
            .ok_or(REFERENCE_SEMANTIC_ERROR)?;

        let next = optional_reference(dictionary, "Next")?;
        if current == last {
            if next.is_some() {
                return Err(REFERENCE_SEMANTIC_ERROR);
            }
            break;
        }
        previous = Some(current);
        current = next.ok_or(REFERENCE_SEMANTIC_ERROR)?;
    }
    Ok(ChainStats {
        direct_items,
        total_items,
        visible_items,
    })
}

fn paired_references(
    dictionary: &Dictionary,
    first: &str,
    last: &str,
) -> Result<(Option<u32>, Option<u32>), &'static str> {
    let first = optional_reference(dictionary, first)?;
    let last = optional_reference(dictionary, last)?;
    if first.is_some() != last.is_some() {
        Err(REFERENCE_SEMANTIC_ERROR)
    } else {
        Ok((first, last))
    }
}

fn name<'a>(dictionary: &'a Dictionary, key: &str) -> Option<&'a str> {
    match dictionary.get(key) {
        Some(ReferenceValue::Name(value)) => Some(value),
        _ => None,
    }
}

fn integer(dictionary: &Dictionary, key: &str) -> Option<i64> {
    match dictionary.get(key) {
        Some(ReferenceValue::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn required_reference(dictionary: &Dictionary, key: &str) -> Option<u32> {
    match dictionary.get(key) {
        Some(ReferenceValue::Reference(value)) => Some(*value),
        _ => None,
    }
}

fn optional_reference(dictionary: &Dictionary, key: &str) -> Result<Option<u32>, &'static str> {
    match dictionary.get(key) {
        None | Some(ReferenceValue::Null) => Ok(None),
        Some(ReferenceValue::Reference(value)) => Ok(Some(*value)),
        Some(_) => Err(REFERENCE_SEMANTIC_ERROR),
    }
}

fn optional_integer(dictionary: &Dictionary, key: &str) -> Result<Option<i64>, &'static str> {
    match dictionary.get(key) {
        None | Some(ReferenceValue::Null) => Ok(None),
        Some(ReferenceValue::Integer(value)) => Ok(Some(*value)),
        Some(_) => Err(REFERENCE_SEMANTIC_ERROR),
    }
}

fn optional_name<'a>(
    dictionary: &'a Dictionary,
    key: &str,
) -> Result<Option<&'a str>, &'static str> {
    match dictionary.get(key) {
        None | Some(ReferenceValue::Null) => Ok(None),
        Some(ReferenceValue::Name(value)) => Ok(Some(value)),
        Some(_) => Err(REFERENCE_SEMANTIC_ERROR),
    }
}

fn optional_destination(dictionary: &Dictionary, key: &str) -> Result<bool, &'static str> {
    match dictionary.get(key) {
        None | Some(ReferenceValue::Null) => Ok(false),
        Some(ReferenceValue::Array(_) | ReferenceValue::Name(_) | ReferenceValue::String(_)) => {
            Ok(true)
        }
        Some(_) => Err(REFERENCE_SEMANTIC_ERROR),
    }
}

fn optional_action(dictionary: &Dictionary, key: &str) -> Result<bool, &'static str> {
    match dictionary.get(key) {
        None | Some(ReferenceValue::Null) => Ok(false),
        Some(ReferenceValue::Dictionary(_)) => Ok(true),
        Some(_) => Err(REFERENCE_SEMANTIC_ERROR),
    }
}

fn references(dictionary: &Dictionary, key: &str) -> Option<Vec<u32>> {
    match dictionary.get(key) {
        Some(ReferenceValue::Array(values)) => values
            .iter()
            .map(|value| match value {
                ReferenceValue::Reference(reference) => Some(*reference),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn string<'a>(dictionary: &'a Dictionary, key: &str) -> Option<&'a str> {
    match dictionary.get(key) {
        Some(ReferenceValue::String(value)) => Some(value),
        _ => None,
    }
}

struct ValueParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> ValueParser<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn parse_complete_dictionary(mut self) -> Result<Dictionary, &'static str> {
        let dictionary = self.parse_dictionary()?;
        self.skip_whitespace();
        if self.cursor != self.bytes.len() {
            return Err(REFERENCE_PARSE_ERROR);
        }
        Ok(dictionary)
    }

    fn parse_dictionary(&mut self) -> Result<Dictionary, &'static str> {
        self.skip_whitespace();
        self.expect(b"<<")?;
        let mut dictionary = BTreeMap::new();
        loop {
            self.skip_whitespace();
            if self.consume(b">>") {
                break;
            }
            let key = self.parse_name()?;
            let value = self.parse_value()?;
            if dictionary.insert(key, value).is_some() {
                return Err(REFERENCE_PARSE_ERROR);
            }
        }
        Ok(dictionary)
    }

    fn parse_value(&mut self) -> Result<ReferenceValue, &'static str> {
        self.skip_whitespace();
        match self.bytes.get(self.cursor).copied() {
            Some(b'n') => self.parse_null(),
            Some(b'/') => Ok(ReferenceValue::Name(self.parse_name()?)),
            Some(b'[') => self.parse_array(),
            Some(b'(') => self.parse_string(),
            Some(b'<') if self.bytes.get(self.cursor + 1) == Some(&b'<') => {
                Ok(ReferenceValue::Dictionary(self.parse_dictionary()?))
            }
            Some(b'-' | b'+' | b'0'..=b'9') => self.parse_number_or_reference(),
            _ => Err(REFERENCE_PARSE_ERROR),
        }
    }

    fn parse_null(&mut self) -> Result<ReferenceValue, &'static str> {
        self.expect(b"null")?;
        Ok(ReferenceValue::Null)
    }

    fn parse_array(&mut self) -> Result<ReferenceValue, &'static str> {
        self.expect(b"[")?;
        let mut values = Vec::new();
        loop {
            self.skip_whitespace();
            if self.consume(b"]") {
                break;
            }
            values.push(self.parse_value()?);
        }
        Ok(ReferenceValue::Array(values))
    }

    fn parse_string(&mut self) -> Result<ReferenceValue, &'static str> {
        self.expect(b"(")?;
        let mut output = String::new();
        let mut depth = 1_u32;
        while let Some(byte) = self.bytes.get(self.cursor).copied() {
            self.cursor += 1;
            match byte {
                b'\\' => {
                    let escaped = self
                        .bytes
                        .get(self.cursor)
                        .copied()
                        .ok_or(REFERENCE_PARSE_ERROR)?;
                    self.cursor += 1;
                    output.push(match escaped {
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{0008}',
                        b'f' => '\u{000c}',
                        other => char::from(other),
                    });
                }
                b'(' => {
                    depth = depth.checked_add(1).ok_or(REFERENCE_PARSE_ERROR)?;
                    output.push('(');
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(ReferenceValue::String(output));
                    }
                    output.push(')');
                }
                other if other.is_ascii() => output.push(char::from(other)),
                _ => return Err(REFERENCE_PARSE_ERROR),
            }
        }
        Err(REFERENCE_PARSE_ERROR)
    }

    fn parse_number_or_reference(&mut self) -> Result<ReferenceValue, &'static str> {
        let first = self.parse_integer()?;
        let restore = self.cursor;
        self.skip_whitespace();
        if let Ok(generation) = self.parse_integer() {
            self.skip_whitespace();
            if generation == 0 && self.consume(b"R") {
                return u32::try_from(first)
                    .map(ReferenceValue::Reference)
                    .map_err(|_| REFERENCE_PARSE_ERROR);
            }
        }
        self.cursor = restore;
        Ok(ReferenceValue::Integer(first))
    }

    fn parse_integer(&mut self) -> Result<i64, &'static str> {
        let start = self.cursor;
        if matches!(self.bytes.get(self.cursor), Some(b'+' | b'-')) {
            self.cursor += 1;
        }
        let digits = self.cursor;
        while self.bytes.get(self.cursor).is_some_and(u8::is_ascii_digit) {
            self.cursor += 1;
        }
        if self.cursor == digits {
            self.cursor = start;
            return Err(REFERENCE_PARSE_ERROR);
        }
        std::str::from_utf8(&self.bytes[start..self.cursor])
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or(REFERENCE_PARSE_ERROR)
    }

    fn parse_name(&mut self) -> Result<String, &'static str> {
        self.expect(b"/")?;
        let start = self.cursor;
        while self.bytes.get(self.cursor).is_some_and(|byte| {
            !byte.is_ascii_whitespace()
                && !matches!(byte, b'/' | b'[' | b']' | b'<' | b'>' | b'(' | b')')
        }) {
            self.cursor += 1;
        }
        if self.cursor == start {
            return Err(REFERENCE_PARSE_ERROR);
        }
        std::str::from_utf8(&self.bytes[start..self.cursor])
            .map(str::to_owned)
            .map_err(|_| REFERENCE_PARSE_ERROR)
    }

    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.cursor += 1;
        }
    }

    fn expect(&mut self, expected: &[u8]) -> Result<(), &'static str> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(REFERENCE_PARSE_ERROR)
        }
    }

    fn consume(&mut self, expected: &[u8]) -> bool {
        if self.bytes.get(self.cursor..self.cursor + expected.len()) == Some(expected) {
            self.cursor += expected.len();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{CASES, CaseKind, ServiceCase, case_contract, repository_input};
    use super::*;

    #[test]
    fn integer_values_cannot_masquerade_as_absent_optional_references() {
        let nested = service_case(CaseKind::NestedValid);
        let page_parent =
            mutated_result(nested, "/Type /Pages /Kids", "/Type /Pages /Parent 7 /Kids");
        assert_eq!(
            page_parent.page_count,
            ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR)
        );

        for (from, to) in [
            ("/Outlines 7 0 R", "/Outlines 7"),
            ("/Parent 7 0 R /Next", "/Parent 7 0 R /Prev 7 /Next"),
            (
                "/Title (Beta) /Parent 7 0 R /Prev 8 0 R",
                "/Title (Beta) /Parent 7 0 R /Prev 8 0 R /Next 7",
            ),
            (
                "/Type /Outlines /First 8 0 R /Last 10 0 R /Count 3",
                "/Type /Outlines /First 8 /Last 10",
            ),
        ] {
            assert_eq!(
                mutated_result(nested, from, to).outline,
                ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR),
                "mutation {from:?} -> {to:?} must remain present and ill-typed"
            );
        }
    }

    #[test]
    fn wrong_optional_count_and_target_shapes_are_not_treated_as_absent_or_valid() {
        let nested = service_case(CaseKind::NestedValid);
        for (from, to) in [
            ("/Last 10 0 R /Count 3", "/Last 10 0 R /Count /Three"),
            (
                "/Title (Beta) /Parent 7 0 R /Prev 8 0 R",
                "/Title (Beta) /Parent 7 0 R /Prev 8 0 R /Dest 7",
            ),
        ] {
            assert_eq!(
                mutated_result(nested, from, to).outline,
                ServiceOutcome::Failed(REFERENCE_SEMANTIC_ERROR)
            );
        }
    }

    #[test]
    fn direct_null_remains_equivalent_to_an_absent_optional_field() {
        let single = service_case(CaseKind::SinglePageNoOutline);
        let result = mutated_result(
            single,
            "/Type /Catalog /Pages 2 0 R",
            "/Type /Catalog /Pages 2 0 R /Outlines null",
        );
        assert!(matches!(result.page_count, ServiceOutcome::Ready(_)));
        assert!(matches!(result.outline, ServiceOutcome::Ready(_)));
    }

    fn service_case(kind: CaseKind) -> ServiceCase {
        CASES
            .into_iter()
            .find(|case| case.kind == kind)
            .expect("requested service case is registered")
    }

    fn mutated_result(case: ServiceCase, from: &str, to: &str) -> ServiceResult {
        let input = String::from_utf8(repository_input(case).to_vec())
            .expect("self-authored fixture is ASCII");
        assert!(
            input.contains(from),
            "mutation source must be unique in fixture"
        );
        let mutated = input.replacen(from, to, 1).into_bytes();
        reference_result(&mutated, &case_contract(case))
    }
}
