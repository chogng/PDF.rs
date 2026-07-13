use std::fmt;

use crate::json::{
    CanonicalJson, push_i64_array, push_json_string, push_number, push_optional_u32,
    push_optional_u64, push_optional_usize, push_u32_array,
};

/// Stable semantic artifact category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactKind {
    /// Parsed object and diagnostic output.
    Parse,
    /// Ordered canonical Scene commands.
    Scene,
    /// Ordered canonical text runs.
    Text,
}

impl ArtifactKind {
    /// Returns the protocol spelling used in summaries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Scene => "scene",
            Self::Text => "text",
        }
    }
}

/// One parsed indirect object represented by stable semantic identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ParseObject {
    object_number: u32,
    generation: u16,
    kind: String,
    semantic_hash: String,
}

impl ParseObject {
    /// Creates a parsed-object record.
    pub fn new(
        object_number: u32,
        generation: u16,
        kind: impl Into<String>,
        semantic_hash: impl Into<String>,
    ) -> Self {
        Self {
            object_number,
            generation,
            kind: kind.into(),
            semantic_hash: semantic_hash.into(),
        }
    }

    /// Returns the PDF object number.
    pub const fn object_number(&self) -> u32 {
        self.object_number
    }

    /// Returns the PDF generation number.
    pub const fn generation(&self) -> u16 {
        self.generation
    }

    /// Returns the canonical object-kind name.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the caller-provided semantic digest.
    pub fn semantic_hash(&self) -> &str {
        &self.semantic_hash
    }
}

impl CanonicalJson for ParseObject {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"generation\":");
        push_number(output, self.generation);
        output.push_str(",\"kind\":");
        push_json_string(output, &self.kind);
        output.push_str(",\"object_number\":");
        push_number(output, self.object_number);
        output.push_str(",\"semantic_hash\":");
        push_json_string(output, &self.semantic_hash);
        output.push('}');
    }
}

/// Stable parse diagnostic without environment-dependent message text.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ParseDiagnostic {
    code: String,
    object_number: Option<u32>,
    byte_offset: Option<u64>,
}

impl ParseDiagnostic {
    /// Creates a diagnostic record.
    pub fn new(
        code: impl Into<String>,
        object_number: Option<u32>,
        byte_offset: Option<u64>,
    ) -> Self {
        Self {
            code: code.into(),
            object_number,
            byte_offset,
        }
    }

    /// Returns the stable diagnostic code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the associated object number, when known.
    pub const fn object_number(&self) -> Option<u32> {
        self.object_number
    }

    /// Returns the associated byte offset, when known.
    pub const fn byte_offset(&self) -> Option<u64> {
        self.byte_offset
    }
}

impl CanonicalJson for ParseDiagnostic {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"byte_offset\":");
        push_optional_u64(output, self.byte_offset);
        output.push_str(",\"code\":");
        push_json_string(output, &self.code);
        output.push_str(",\"object_number\":");
        push_optional_u32(output, self.object_number);
        output.push('}');
    }
}

/// Canonical parse artifact.
///
/// Objects and diagnostics are sorted at construction because their discovery
/// order is not part of the PDF semantic result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseArtifact {
    schema: u32,
    objects: Vec<ParseObject>,
    diagnostics: Vec<ParseDiagnostic>,
}

impl ParseArtifact {
    /// Creates and canonicalizes a parse artifact.
    pub fn new(
        schema: u32,
        mut objects: Vec<ParseObject>,
        mut diagnostics: Vec<ParseDiagnostic>,
    ) -> Self {
        objects.sort();
        diagnostics.sort();
        Self {
            schema,
            objects,
            diagnostics,
        }
    }

    /// Returns the artifact schema version.
    pub const fn schema(&self) -> u32 {
        self.schema
    }

    /// Returns canonical parsed objects.
    pub fn objects(&self) -> &[ParseObject] {
        &self.objects
    }

    /// Returns canonical diagnostics.
    pub fn diagnostics(&self) -> &[ParseDiagnostic] {
        &self.diagnostics
    }
}

impl CanonicalJson for ParseArtifact {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"diagnostics\":[");
        push_json_values(output, &self.diagnostics);
        output.push_str("],\"objects\":[");
        push_json_values(output, &self.objects);
        output.push_str("],\"schema\":");
        push_number(output, self.schema);
        output.push('}');
    }
}

/// One ordered canonical Scene command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneCommand {
    kind: String,
    semantic_hash: String,
    source_object: Option<u32>,
    transform_microunits: [i64; 6],
}

impl SceneCommand {
    /// Creates a Scene command using fixed-point transform components.
    pub fn new(
        kind: impl Into<String>,
        semantic_hash: impl Into<String>,
        source_object: Option<u32>,
        transform_microunits: [i64; 6],
    ) -> Self {
        Self {
            kind: kind.into(),
            semantic_hash: semantic_hash.into(),
            source_object,
            transform_microunits,
        }
    }

    /// Returns the canonical command-kind name.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the semantic digest for command payload and resources.
    pub fn semantic_hash(&self) -> &str {
        &self.semantic_hash
    }

    /// Returns the originating PDF object, when known.
    pub const fn source_object(&self) -> Option<u32> {
        self.source_object
    }

    /// Returns the six fixed-point transform components.
    pub const fn transform_microunits(&self) -> &[i64; 6] {
        &self.transform_microunits
    }
}

impl CanonicalJson for SceneCommand {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"kind\":");
        push_json_string(output, &self.kind);
        output.push_str(",\"semantic_hash\":");
        push_json_string(output, &self.semantic_hash);
        output.push_str(",\"source_object\":");
        push_optional_u32(output, self.source_object);
        output.push_str(",\"transform_microunits\":");
        push_i64_array(output, &self.transform_microunits);
        output.push('}');
    }
}

/// Canonical ordered Scene artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneArtifact {
    schema: u32,
    commands: Vec<SceneCommand>,
}

impl SceneArtifact {
    /// Creates a Scene artifact. Command order is preserved and compared exactly.
    pub fn new(schema: u32, commands: Vec<SceneCommand>) -> Self {
        Self { schema, commands }
    }

    /// Returns the artifact schema version.
    pub const fn schema(&self) -> u32 {
        self.schema
    }

    /// Returns ordered Scene commands.
    pub fn commands(&self) -> &[SceneCommand] {
        &self.commands
    }
}

impl CanonicalJson for SceneArtifact {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"commands\":[");
        push_json_values(output, &self.commands);
        output.push_str("],\"schema\":");
        push_number(output, self.schema);
        output.push('}');
    }
}

/// Canonical writing-mode classification for text comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WritingMode {
    /// Horizontal left-to-right layout.
    HorizontalLtr,
    /// Horizontal right-to-left layout.
    HorizontalRtl,
    /// Vertical layout.
    Vertical,
}

impl WritingMode {
    /// Returns the protocol spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HorizontalLtr => "horizontal-ltr",
            Self::HorizontalRtl => "horizontal-rtl",
            Self::Vertical => "vertical",
        }
    }
}

/// One ordered canonical text run.
#[derive(Clone, Eq, PartialEq)]
pub struct TextRun {
    glyph_ids: Vec<u32>,
    quad_micropoints: [i64; 8],
    unicode: String,
    writing_mode: WritingMode,
}

impl fmt::Debug for TextRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TextRun")
            .field("glyph_count", &self.glyph_ids.len())
            .field("writing_mode", &self.writing_mode)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl TextRun {
    /// Creates a canonical text run.
    pub fn new(
        glyph_ids: Vec<u32>,
        quad_micropoints: [i64; 8],
        unicode: impl Into<String>,
        writing_mode: WritingMode,
    ) -> Self {
        Self {
            glyph_ids,
            quad_micropoints,
            unicode: unicode.into(),
            writing_mode,
        }
    }

    /// Returns ordered glyph identifiers.
    pub fn glyph_ids(&self) -> &[u32] {
        &self.glyph_ids
    }

    /// Returns the fixed-point quadrilateral coordinates.
    pub const fn quad_micropoints(&self) -> &[i64; 8] {
        &self.quad_micropoints
    }

    /// Returns mapped Unicode text.
    pub fn unicode(&self) -> &str {
        &self.unicode
    }

    /// Returns the run writing mode.
    pub const fn writing_mode(&self) -> WritingMode {
        self.writing_mode
    }
}

impl CanonicalJson for TextRun {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"glyph_ids\":");
        push_u32_array(output, &self.glyph_ids);
        output.push_str(",\"quad_micropoints\":");
        push_i64_array(output, &self.quad_micropoints);
        output.push_str(",\"unicode\":");
        push_json_string(output, &self.unicode);
        output.push_str(",\"writing_mode\":");
        push_json_string(output, self.writing_mode.as_str());
        output.push('}');
    }
}

/// Canonical ordered text artifact.
#[derive(Clone, Eq, PartialEq)]
pub struct TextArtifact {
    schema: u32,
    runs: Vec<TextRun>,
}

impl fmt::Debug for TextArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TextArtifact")
            .field("schema", &self.schema)
            .field("run_count", &self.runs.len())
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl TextArtifact {
    /// Creates a text artifact. Run order is preserved and compared exactly.
    pub fn new(schema: u32, runs: Vec<TextRun>) -> Self {
        Self { schema, runs }
    }

    /// Returns the artifact schema version.
    pub const fn schema(&self) -> u32 {
        self.schema
    }

    /// Returns ordered text runs.
    pub fn runs(&self) -> &[TextRun] {
        &self.runs
    }
}

impl CanonicalJson for TextArtifact {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"runs\":[");
        push_json_values(output, &self.runs);
        output.push_str("],\"schema\":");
        push_number(output, self.schema);
        output.push('}');
    }
}

/// Exact comparison counts for one ordered artifact section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionDiff {
    name: &'static str,
    expected_records: usize,
    actual_records: usize,
    changed_records: usize,
    missing_records: usize,
    unexpected_records: usize,
    first_difference: Option<usize>,
}

impl SectionDiff {
    /// Returns the section name.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the expected record count.
    pub const fn expected_records(&self) -> usize {
        self.expected_records
    }

    /// Returns the actual record count.
    pub const fn actual_records(&self) -> usize {
        self.actual_records
    }

    /// Returns the number of aligned records that differ.
    pub const fn changed_records(&self) -> usize {
        self.changed_records
    }

    /// Returns the number of missing expected tail records.
    pub const fn missing_records(&self) -> usize {
        self.missing_records
    }

    /// Returns the number of unexpected actual tail records.
    pub const fn unexpected_records(&self) -> usize {
        self.unexpected_records
    }

    /// Returns the first differing aligned index or first length boundary.
    pub const fn first_difference(&self) -> Option<usize> {
        self.first_difference
    }

    /// Reports whether this section is exactly equal.
    pub const fn is_exact(&self) -> bool {
        self.changed_records == 0 && self.missing_records == 0 && self.unexpected_records == 0
    }
}

impl CanonicalJson for SectionDiff {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"actual_records\":");
        push_number(output, self.actual_records);
        output.push_str(",\"changed_records\":");
        push_number(output, self.changed_records);
        output.push_str(",\"exact\":");
        output.push_str(if self.is_exact() { "true" } else { "false" });
        output.push_str(",\"expected_records\":");
        push_number(output, self.expected_records);
        output.push_str(",\"first_difference\":");
        push_optional_usize(output, self.first_difference);
        output.push_str(",\"missing_records\":");
        push_number(output, self.missing_records);
        output.push_str(",\"name\":");
        push_json_string(output, self.name);
        output.push_str(",\"unexpected_records\":");
        push_number(output, self.unexpected_records);
        output.push('}');
    }
}

/// Deterministic exact semantic comparison summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticDiffSummary {
    artifact: ArtifactKind,
    metadata_differences: usize,
    sections: Vec<SectionDiff>,
}

impl SemanticDiffSummary {
    /// Returns the compared artifact category.
    pub const fn artifact(&self) -> ArtifactKind {
        self.artifact
    }

    /// Returns the count of differing top-level metadata fields.
    pub const fn metadata_differences(&self) -> usize {
        self.metadata_differences
    }

    /// Returns per-section exact counts.
    pub fn sections(&self) -> &[SectionDiff] {
        &self.sections
    }

    /// Reports whether metadata and every record are exactly equal.
    pub fn is_exact(&self) -> bool {
        self.metadata_differences == 0 && self.sections.iter().all(SectionDiff::is_exact)
    }
}

impl CanonicalJson for SemanticDiffSummary {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"artifact\":");
        push_json_string(output, self.artifact.as_str());
        output.push_str(",\"exact\":");
        output.push_str(if self.is_exact() { "true" } else { "false" });
        output.push_str(",\"metadata_differences\":");
        push_number(output, self.metadata_differences);
        output.push_str(",\"sections\":[");
        push_json_values(output, &self.sections);
        output.push_str("]}");
    }
}

/// Compares canonical parse output without tolerance.
pub fn compare_parse(expected: &ParseArtifact, actual: &ParseArtifact) -> SemanticDiffSummary {
    SemanticDiffSummary {
        artifact: ArtifactKind::Parse,
        metadata_differences: usize::from(expected.schema != actual.schema),
        sections: vec![
            compare_section("diagnostics", &expected.diagnostics, &actual.diagnostics),
            compare_section("objects", &expected.objects, &actual.objects),
        ],
    }
}

/// Compares ordered canonical Scene commands without tolerance.
pub fn compare_scene(expected: &SceneArtifact, actual: &SceneArtifact) -> SemanticDiffSummary {
    SemanticDiffSummary {
        artifact: ArtifactKind::Scene,
        metadata_differences: usize::from(expected.schema != actual.schema),
        sections: vec![compare_section(
            "commands",
            &expected.commands,
            &actual.commands,
        )],
    }
}

/// Compares ordered canonical text runs without tolerance.
pub fn compare_text(expected: &TextArtifact, actual: &TextArtifact) -> SemanticDiffSummary {
    SemanticDiffSummary {
        artifact: ArtifactKind::Text,
        metadata_differences: usize::from(expected.schema != actual.schema),
        sections: vec![compare_section("runs", &expected.runs, &actual.runs)],
    }
}

fn compare_section<T: PartialEq>(name: &'static str, expected: &[T], actual: &[T]) -> SectionDiff {
    let shared = expected.len().min(actual.len());
    let mut changed_records = 0usize;
    let mut first_difference = None;

    for (index, (expected_record, actual_record)) in expected.iter().zip(actual.iter()).enumerate()
    {
        if expected_record != actual_record {
            changed_records += 1;
            first_difference.get_or_insert(index);
        }
    }

    if first_difference.is_none() && expected.len() != actual.len() {
        first_difference = Some(shared);
    }

    SectionDiff {
        name,
        expected_records: expected.len(),
        actual_records: actual.len(),
        changed_records,
        missing_records: expected.len().saturating_sub(actual.len()),
        unexpected_records: actual.len().saturating_sub(expected.len()),
        first_difference,
    }
}

fn push_json_values<T: CanonicalJson>(output: &mut String, values: &[T]) {
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        value.write_canonical_json(output);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ParseArtifact, ParseDiagnostic, ParseObject, SceneArtifact, SceneCommand, TextArtifact,
        TextRun, WritingMode, compare_parse, compare_scene, compare_text,
    };
    use crate::CanonicalJson;

    #[test]
    fn parse_artifact_sorts_unordered_semantic_records() {
        let artifact = ParseArtifact::new(
            1,
            vec![
                ParseObject::new(7, 0, "stream", "sha256:b"),
                ParseObject::new(2, 0, "dict", "sha256:a"),
            ],
            vec![
                ParseDiagnostic::new("RPE-Z", Some(7), Some(18)),
                ParseDiagnostic::new("RPE-A", None, None),
            ],
        );

        assert_eq!(
            artifact.to_canonical_json(),
            "{\"diagnostics\":[{\"byte_offset\":null,\"code\":\"RPE-A\",\"object_number\":null},{\"byte_offset\":18,\"code\":\"RPE-Z\",\"object_number\":7}],\"objects\":[{\"generation\":0,\"kind\":\"dict\",\"object_number\":2,\"semantic_hash\":\"sha256:a\"},{\"generation\":0,\"kind\":\"stream\",\"object_number\":7,\"semantic_hash\":\"sha256:b\"}],\"schema\":1}"
        );
    }

    #[test]
    fn exact_and_changed_parse_summaries_are_stable() {
        let expected = ParseArtifact::new(1, vec![ParseObject::new(1, 0, "dict", "a")], Vec::new());
        assert!(compare_parse(&expected, &expected).is_exact());

        let actual = ParseArtifact::new(
            2,
            vec![
                ParseObject::new(1, 0, "dict", "b"),
                ParseObject::new(2, 0, "array", "c"),
            ],
            Vec::new(),
        );
        let summary = compare_parse(&expected, &actual);
        assert!(!summary.is_exact());
        assert_eq!(summary.metadata_differences(), 1);
        assert_eq!(summary.sections()[1].changed_records(), 1);
        assert_eq!(summary.sections()[1].unexpected_records(), 1);
        assert_eq!(summary.sections()[1].first_difference(), Some(0));
    }

    #[test]
    fn scene_command_order_and_payload_are_exact() {
        let first = SceneCommand::new("fill", "path:a", Some(4), [1, 0, 0, 1, 0, 0]);
        let second = SceneCommand::new("stroke", "path:b", Some(4), [1, 0, 0, 1, 0, 0]);
        let expected = SceneArtifact::new(1, vec![first.clone(), second.clone()]);
        let reordered = SceneArtifact::new(1, vec![second, first]);

        let summary = compare_scene(&expected, &reordered);
        assert_eq!(summary.sections()[0].changed_records(), 2);
        assert_eq!(summary.sections()[0].first_difference(), Some(0));
    }

    #[test]
    fn text_comparison_preserves_unicode_and_run_order() {
        let expected = TextArtifact::new(
            1,
            vec![TextRun::new(
                vec![10, 11],
                [0, 0, 2, 0, 2, 1, 0, 1],
                "A\n文",
                WritingMode::HorizontalLtr,
            )],
        );
        let actual = TextArtifact::new(
            1,
            vec![TextRun::new(
                vec![10, 12],
                [0, 0, 2, 0, 2, 1, 0, 1],
                "A\n文",
                WritingMode::HorizontalLtr,
            )],
        );

        assert!(expected.to_canonical_json().contains("A\\n文"));
        assert_eq!(
            compare_text(&expected, &actual).sections()[0].changed_records(),
            1
        );
    }

    #[test]
    fn text_debug_output_redacts_document_content() {
        let artifact = TextArtifact::new(
            1,
            vec![TextRun::new(
                vec![1, 2],
                [0; 8],
                "private-document-text",
                WritingMode::HorizontalLtr,
            )],
        );
        let debug = format!("{artifact:?}");
        assert!(!debug.contains("private-document-text"));
        assert!(!debug.contains("[1, 2]"));
        assert!(debug.contains("[REDACTED]"));
    }
}
