use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_cache::{ReadyStoreBinding, ReadyStoreEpoch, ReadyStoreKey, ReadyStoreSessionId};
use pdf_rs_document::{
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex,
    NeverCancelled as DocumentNeverCancelled, ReferenceChainJobContext, ReferenceChainLimits,
    ReferenceChainPoll, ResolveReferenceChainJob, ResolvedReference, RevisionAttestationJobContext,
    RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_syntax::ObjectRef;
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
};

pub(crate) const SESSION_ID: ReadyStoreSessionId = ReadyStoreSessionId::new(0x05e5_510a);
pub(crate) const EPOCH: ReadyStoreEpoch = ReadyStoreEpoch::new(7);

pub(crate) struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

impl Fixture {
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }
}

fn snapshot(len: u64, seed: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([seed; 32]),
            SourceRevision::new(u64::from(seed)),
        ),
        Some(len),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [seed.wrapping_add(1); 32],
        ),
    )
}

pub(crate) fn fixture(seed: u8) -> Fixture {
    let bodies: &[(u32, &[u8])] = &[
        (1, b"1 0 obj\n<< /Name (SESSION_SECRET_VALUE) >>\nendobj\n"),
        (2, b"2 0 obj\n1 0 R\nendobj\n"),
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for &(number, body) in bodies {
        offsets.push((
            number,
            u64::try_from(bytes.len()).expect("fixture offset fits u64"),
        ));
        bytes.extend_from_slice(body);
    }

    let startxref = u64::try_from(bytes.len()).expect("fixture length fits u64");
    let size = 3_u32;
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for number in 0..size {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else {
            let offset = offsets
                .iter()
                .find(|(candidate, _)| *candidate == number)
                .map(|(_, offset)| *offset)
                .expect("every nonzero fixture object is in use");
            format!("{offset:010} 00000 n \n")
        };
        assert_eq!(row.len(), 20);
        bytes.extend_from_slice(row.as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
            .as_bytes(),
    );

    let source = snapshot(
        u64::try_from(bytes.len()).expect("fixture length fits u64"),
        seed,
    );
    Fixture {
        bytes,
        snapshot: source,
    }
}

pub(crate) fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("test references are nonzero")
}

pub(crate) fn store_for(fixture: &Fixture) -> RangeStore {
    let store =
        RangeStore::new(fixture.snapshot(), Default::default()).expect("store limits validate");
    let range = ByteRange::new(
        0,
        u64::try_from(fixture.bytes().len()).expect("fixture length fits u64"),
    )
    .expect("fixture range is nonempty");
    store
        .supply(
            RangeResponse::new(fixture.snapshot(), range, fixture.bytes().to_vec())
                .expect("fixture response matches its range"),
        )
        .expect("fixture fits byte-store limits");
    store
}

pub(crate) fn ready_index(fixture: &Fixture) -> AttestedRevisionIndex {
    let store = store_for(fixture);
    let mut xref = OpenXrefJob::new(
        fixture.snapshot(),
        XrefJobContext::new(
            JobId::new(101),
            ResumeCheckpoint::new(102),
            ResumeCheckpoint::new(103),
        ),
        XrefLimits::default(),
        Default::default(),
    )
    .expect("xref job validates");
    let section = match xref.poll(&store, &XrefNeverCancelled) {
        XrefPoll::Ready(section) => section,
        XrefPoll::Pending { .. } => panic!("complete xref input must not suspend"),
        XrefPoll::Failed(error) => panic!("self-authored xref must parse: {error}"),
    };
    let candidate = CandidateRevisionIndex::from_xref(
        &section,
        RevisionId::new(41),
        Default::default(),
        &DocumentNeverCancelled,
    )
    .expect("self-authored xref yields a candidate");
    let mut attest = AttestRevisionJob::new(
        candidate,
        RevisionAttestationJobContext::new(
            JobId::new(201),
            ResumeCheckpoint::new(202),
            ResumeCheckpoint::new(203),
            ResumeCheckpoint::new(204),
            RequestPriority::Metadata,
        ),
        RevisionAttestationLimits::default(),
        Default::default(),
        Default::default(),
    )
    .expect("attestation job validates");
    match attest.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Ready(index) => index,
        RevisionAttestationPoll::Pending { .. } => {
            panic!("complete attestation input must not suspend")
        }
        RevisionAttestationPoll::Failed(error) => {
            panic!("self-authored revision must attest: {error}")
        }
    }
}

pub(crate) fn binding(index: &AttestedRevisionIndex) -> ReadyStoreBinding {
    ReadyStoreBinding::for_index(index, SESSION_ID, EPOCH)
}

pub(crate) fn key(
    binding: ReadyStoreBinding,
    root: ObjectRef,
    limits: ReferenceChainLimits,
) -> ReadyStoreKey {
    ReadyStoreKey::new(binding, root, limits)
}

pub(crate) fn resolve(
    index: &AttestedRevisionIndex,
    store: &RangeStore,
    root: ObjectRef,
    limits: ReferenceChainLimits,
) -> ResolvedReference {
    let mut job: ResolveReferenceChainJob<'_> = index
        .resolve_reference_chain(
            root,
            ReferenceChainJobContext::new(
                JobId::new(301),
                ResumeCheckpoint::new(302),
                ResumeCheckpoint::new(303),
                RequestPriority::VisiblePage,
            ),
            limits,
        )
        .expect("resolution job validates");
    match job.poll(store, &DocumentNeverCancelled) {
        ReferenceChainPoll::Ready(value) => value,
        ReferenceChainPoll::Pending { .. } => panic!("complete resolution input must not suspend"),
        ReferenceChainPoll::Failed(error) => panic!("self-authored chain must resolve: {error}"),
    }
}
