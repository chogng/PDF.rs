use pdf_rs_syntax::ObjectRef;

use crate::{
    CapabilityDecision, CommandSource, PageGeometry, Scene, SceneCommand, SceneCommandKind,
    SceneError, SceneErrorCode, SceneFeature, SceneLimitKind, SceneRect, SceneResource,
    SceneResourceKind,
};

impl Scene {
    /// Serializes this Scene into compact deterministic schema-1 JSON bytes.
    ///
    /// Object fields use fixed lexical order, semantic arrays retain their declared order, PDF
    /// name bytes use lowercase hexadecimal, and numeric values use scaled integers. Runtime
    /// [`pdf_rs_bytes::SourceIdentity`] is deliberately omitted.
    pub fn canonical_json_bytes(&self) -> Result<Vec<u8>, SceneError> {
        if self.commands().len() != self.provenance().len() {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidProvenance,
                None,
            ));
        }
        let mut writer = CanonicalWriter::new(
            self.limits().max_canonical_bytes(),
            SceneLimitKind::CanonicalBytes,
        );
        writer.push(b"{\"binding\":{\"page_index\":")?;
        writer.push_u32(self.binding().page_index())?;
        writer.push(b",\"page_object\":")?;
        write_object_ref(&mut writer, self.binding().page_object())?;
        writer.push(b",\"revision_startxref\":")?;
        writer.push_u64(self.binding().revision_startxref())?;
        writer.push(b"},\"commands\":[")?;
        for (index, command) in self.commands().iter().enumerate() {
            writer.separator(index)?;
            write_command(&mut writer, command)?;
        }
        writer.push(b"],\"features\":{\"decision\":")?;
        match self.features().decision() {
            CapabilityDecision::Supported => writer.push(b"\"supported\"")?,
        }
        writer.push(b",\"tags\":[")?;
        for (index, feature) in self.features().tags().iter().copied().enumerate() {
            writer.separator(index)?;
            writer.push(match feature {
                SceneFeature::MarkedContent => b"\"marked-content\"",
                SceneFeature::MarkedContentProperties => b"\"marked-content-properties\"",
            })?;
        }
        writer.push(b"]},\"geometry\":")?;
        write_geometry(&mut writer, self.geometry())?;
        writer.push(b",\"provenance\":[")?;
        for (index, source) in self.provenance().iter().copied().enumerate() {
            writer.separator(index)?;
            write_source(&mut writer, source)?;
        }
        writer.push(b"],\"resources\":[")?;
        for (index, resource) in self.resources().iter().copied().enumerate() {
            writer.separator(index)?;
            write_resource(&mut writer, resource)?;
        }
        writer.push(b"],\"schema\":{\"major\":")?;
        writer.push_u16(self.version().major())?;
        writer.push(b",\"minor\":")?;
        writer.push_u16(self.version().minor())?;
        writer.push(b"}}")?;
        Ok(writer.finish())
    }
}

fn write_command(writer: &mut CanonicalWriter, command: &SceneCommand) -> Result<(), SceneError> {
    match command.kind() {
        SceneCommandKind::BeginMarkedContent => {
            let tag = command
                .tag()
                .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
            writer.push(b"{\"kind\":\"begin-marked-content\",\"properties\":")?;
            if let Some(resource) = command.properties() {
                writer.push_u32(resource.value())?;
            } else {
                writer.push(b"null")?;
            }
            writer.push(b",\"tag_hex\":\"")?;
            writer.push_hex(tag.bytes())?;
            writer.push(b"\"}")
        }
        SceneCommandKind::EndMarkedContent => writer.push(b"{\"kind\":\"end-marked-content\"}"),
    }
}

fn write_geometry(writer: &mut CanonicalWriter, geometry: PageGeometry) -> Result<(), SceneError> {
    writer.push(b"{\"crop_box\":")?;
    write_rect(writer, geometry.crop_box())?;
    writer.push(b",\"media_box\":")?;
    write_rect(writer, geometry.media_box())?;
    writer.push(b",\"rotation\":")?;
    writer.push_u16(geometry.rotation().degrees())?;
    writer.push(b"}")
}

fn write_rect(writer: &mut CanonicalWriter, rect: SceneRect) -> Result<(), SceneError> {
    writer.push(b"[")?;
    for (index, value) in rect.coordinates().iter().copied().enumerate() {
        writer.separator(index)?;
        writer.push_i64(value.scaled())?;
    }
    writer.push(b"]")
}

fn write_source(writer: &mut CanonicalWriter, source: CommandSource) -> Result<(), SceneError> {
    writer.push(b"{\"decoded_length\":")?;
    writer.push_u64(source.decoded_length())?;
    writer.push(b",\"decoded_start\":")?;
    writer.push_u64(source.decoded_start())?;
    writer.push(b",\"object\":")?;
    write_object_ref(writer, source.object())?;
    writer.push(b",\"operator_index\":")?;
    writer.push_u32(source.operator_index())?;
    writer.push(b",\"stream_index\":")?;
    writer.push_u32(source.stream_index())?;
    writer.push(b"}")
}

fn write_resource(writer: &mut CanonicalWriter, resource: SceneResource) -> Result<(), SceneError> {
    writer.push(b"{\"id\":")?;
    writer.push_u32(resource.id().value())?;
    writer.push(b",\"kind\":")?;
    match resource.kind() {
        SceneResourceKind::MarkedContentProperties => {
            writer.push(b"\"marked-content-properties\"")?;
        }
    }
    writer.push(b",\"object\":")?;
    write_object_ref(writer, resource.object())?;
    writer.push(b"}")
}

fn write_object_ref(writer: &mut CanonicalWriter, reference: ObjectRef) -> Result<(), SceneError> {
    writer.push(b"{\"generation\":")?;
    writer.push_u16(reference.generation())?;
    writer.push(b",\"number\":")?;
    writer.push_u32(reference.number())?;
    writer.push(b"}")
}

pub(crate) struct CanonicalWriter {
    bytes: Vec<u8>,
    limit: u64,
    limit_kind: SceneLimitKind,
}

impl CanonicalWriter {
    pub(crate) const fn new(limit: u64, limit_kind: SceneLimitKind) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            limit_kind,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<(), SceneError> {
        self.reserve_output(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn reserve_output(&mut self, additional: usize) -> Result<(), SceneError> {
        let consumed = u64::try_from(self.bytes.len())
            .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        let attempted = u64::try_from(additional).unwrap_or(u64::MAX);
        let next = consumed.checked_add(attempted).ok_or_else(|| {
            SceneError::resource(self.limit_kind, self.limit, consumed, attempted, None)
        })?;
        if next > self.limit {
            return Err(SceneError::resource(
                self.limit_kind,
                self.limit,
                consumed,
                attempted,
                None,
            ));
        }
        let required = self
            .bytes
            .len()
            .checked_add(additional)
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        if required <= self.bytes.capacity() {
            return Ok(());
        }
        let limit = usize::try_from(self.limit)
            .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        let grown = if self.bytes.capacity() == 0 {
            64
        } else {
            self.bytes.capacity().saturating_mul(2)
        };
        let target = grown.max(required).min(limit);
        let reserve = target
            .checked_sub(self.bytes.len())
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        self.bytes.try_reserve_exact(reserve).map_err(|_| {
            SceneError::resource(
                SceneLimitKind::Allocation,
                self.limit,
                consumed,
                attempted,
                None,
            )
        })?;
        Ok(())
    }

    pub(crate) fn separator(&mut self, index: usize) -> Result<(), SceneError> {
        if index != 0 {
            self.push(b",")?;
        }
        Ok(())
    }

    pub(crate) fn push_u16(&mut self, value: u16) -> Result<(), SceneError> {
        self.push_u64(u64::from(value))
    }

    pub(crate) fn push_u32(&mut self, value: u32) -> Result<(), SceneError> {
        self.push_u64(u64::from(value))
    }

    fn push_u64(&mut self, mut value: u64) -> Result<(), SceneError> {
        let mut buffer = [0_u8; 20];
        let mut index = buffer.len();
        loop {
            index -= 1;
            buffer[index] = b'0' + u8::try_from(value % 10).expect("one digit fits u8");
            value /= 10;
            if value == 0 {
                return self.push(&buffer[index..]);
            }
        }
    }

    fn push_i64(&mut self, value: i64) -> Result<(), SceneError> {
        if value < 0 {
            self.push(b"-")?;
            self.push_u64(value.unsigned_abs())
        } else {
            self.push_u64(value as u64)
        }
    }

    fn push_hex(&mut self, bytes: &[u8]) -> Result<(), SceneError> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let encoded_len = bytes.len().checked_mul(2).ok_or_else(|| {
            SceneError::resource(
                self.limit_kind,
                self.limit,
                u64::try_from(self.bytes.len()).unwrap_or(u64::MAX),
                u64::MAX,
                None,
            )
        })?;
        self.reserve_output(encoded_len)?;
        for byte in bytes {
            let encoded = [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0x0f)]];
            self.bytes.extend_from_slice(&encoded);
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}
