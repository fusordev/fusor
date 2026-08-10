#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InternalStackValue {
    Ordinary,
    DerivedActiveConstructor(BytecodePc),
    DerivedSuperConstructor(BytecodePc),
    DerivedSuperNewTarget(BytecodePc),
    DerivedSuperResult(BytecodePc),
    DerivedSuperCompletion(BytecodePc),
    ForInIterator(BytecodePc),
    ForInKey(BytecodePc),
    ForInDone(BytecodePc),
    ForInHeadKey(BytecodePc),
    ForOfIterator(BytecodePc),
    ForOfNextMethod(BytecodePc),
    ForOfCatch(BytecodePc),
    ForOfDisabledCatch(BytecodePc),
    ForOfAwaitResult(BytecodePc),
    ForOfAwaitedResult(BytecodePc),
    ForOfExhaustedIterator(BytecodePc),
    ForOfExhaustedNextMethod(BytecodePc),
    ForOfExhaustedCatch(BytecodePc),
    ForOfClosableIterator(BytecodePc),
    ForOfClosableNextMethod(BytecodePc),
    ForOfClosableCatch(BytecodePc),
    ForOfValue(BytecodePc),
    ForOfDone(BytecodePc),
    ForOfHeadValue(BytecodePc),
    ForOfReturnValue(BytecodePc),
    ForOfCloseIterator(BytecodePc),
    ForOfCloseNextMethod(BytecodePc),
    ForOfCloseDummy(BytecodePc),
    YieldStarIterator(BytecodePc),
    YieldStarNextMethod(BytecodePc),
    YieldStarDummy(BytecodePc),
    YieldStarIteratorResult(BytecodePc),
    YieldStarDone(BytecodePc),
    YieldStarYieldResult(BytecodePc),
    YieldStarYieldValue(BytecodePc),
    YieldStarFinalResult(BytecodePc),
    YieldStarResumeValue(BytecodePc),
    YieldStarResumeMode(BytecodePc),
    YieldStarResumeModeTest(BytecodePc),
    YieldStarIsThrow(BytecodePc),
    YieldStarCallValue(BytecodePc, YieldStarCallKind),
    YieldStarMethodMissing(BytecodePc, YieldStarCallKind),
    CatchMarker {
        site: BytecodePc,
        handler: InstructionIndex,
    },
    CatchException(BytecodePc),
    FinallyPending {
        target: InstructionIndex,
        original: JavaScriptStackValue,
    },
    FinallyReturn {
        target: InstructionIndex,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum YieldStarCallKind {
    Return,
    Throw,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JavaScriptStackValue {
    Ordinary,
    ForInKey(BytecodePc),
    ForInDone(BytecodePc),
    ForInHeadKey(BytecodePc),
    ForOfValue(BytecodePc),
    ForOfDone(BytecodePc),
    ForOfHeadValue(BytecodePc),
    ForOfReturnValue(BytecodePc),
    CatchException(BytecodePc),
}

impl JavaScriptStackValue {
    const fn from_internal(value: InternalStackValue) -> Option<Self> {
        match value {
            InternalStackValue::ForInKey(site) => Some(Self::ForInKey(site)),
            InternalStackValue::ForInDone(site) => Some(Self::ForInDone(site)),
            InternalStackValue::ForInHeadKey(site) => Some(Self::ForInHeadKey(site)),
            InternalStackValue::ForOfValue(site) => Some(Self::ForOfValue(site)),
            InternalStackValue::ForOfDone(site) => Some(Self::ForOfDone(site)),
            InternalStackValue::ForOfHeadValue(site) => Some(Self::ForOfHeadValue(site)),
            InternalStackValue::ForOfReturnValue(site) => Some(Self::ForOfReturnValue(site)),
            InternalStackValue::CatchException(site) => Some(Self::CatchException(site)),
            InternalStackValue::ForInIterator(_)
            | InternalStackValue::DerivedActiveConstructor(_)
            | InternalStackValue::DerivedSuperConstructor(_)
            | InternalStackValue::DerivedSuperNewTarget(_)
            | InternalStackValue::DerivedSuperCompletion(_)
            | InternalStackValue::ForOfIterator(_)
            | InternalStackValue::ForOfNextMethod(_)
            | InternalStackValue::ForOfCatch(_)
            | InternalStackValue::ForOfDisabledCatch(_)
            | InternalStackValue::ForOfExhaustedIterator(_)
            | InternalStackValue::ForOfExhaustedNextMethod(_)
            | InternalStackValue::ForOfExhaustedCatch(_)
            | InternalStackValue::ForOfClosableIterator(_)
            | InternalStackValue::ForOfClosableNextMethod(_)
            | InternalStackValue::ForOfClosableCatch(_)
            | InternalStackValue::ForOfCloseIterator(_)
            | InternalStackValue::ForOfCloseNextMethod(_)
            | InternalStackValue::ForOfCloseDummy(_)
            | InternalStackValue::YieldStarIterator(_)
            | InternalStackValue::YieldStarNextMethod(_)
            | InternalStackValue::YieldStarDummy(_)
            | InternalStackValue::CatchMarker { .. }
            | InternalStackValue::FinallyPending { .. }
            | InternalStackValue::FinallyReturn { .. } => None,
            InternalStackValue::Ordinary
            | InternalStackValue::DerivedSuperResult(_)
            | InternalStackValue::ForOfAwaitResult(_)
            | InternalStackValue::ForOfAwaitedResult(_)
            | InternalStackValue::YieldStarIteratorResult(_)
            | InternalStackValue::YieldStarDone(_)
            | InternalStackValue::YieldStarYieldResult(_)
            | InternalStackValue::YieldStarYieldValue(_)
            | InternalStackValue::YieldStarFinalResult(_)
            | InternalStackValue::YieldStarResumeValue(_)
            | InternalStackValue::YieldStarResumeMode(_)
            | InternalStackValue::YieldStarResumeModeTest(_)
            | InternalStackValue::YieldStarIsThrow(_)
            | InternalStackValue::YieldStarCallValue(_, _)
            | InternalStackValue::YieldStarMethodMissing(_, _) => Some(Self::Ordinary),
        }
    }

    const fn into_internal(self) -> InternalStackValue {
        match self {
            Self::Ordinary => InternalStackValue::Ordinary,
            Self::ForInKey(site) => InternalStackValue::ForInKey(site),
            Self::ForInDone(site) => InternalStackValue::ForInDone(site),
            Self::ForInHeadKey(site) => InternalStackValue::ForInHeadKey(site),
            Self::ForOfValue(site) => InternalStackValue::ForOfValue(site),
            Self::ForOfDone(site) => InternalStackValue::ForOfDone(site),
            Self::ForOfHeadValue(site) => InternalStackValue::ForOfHeadValue(site),
            Self::ForOfReturnValue(site) => InternalStackValue::ForOfReturnValue(site),
            Self::CatchException(site) => InternalStackValue::CatchException(site),
        }
    }
}
impl InternalStackValue {
    const fn is_javascript_value(self) -> bool {
        !matches!(
            self,
            Self::ForInIterator(_)
                | Self::DerivedActiveConstructor(_)
                | Self::DerivedSuperConstructor(_)
                | Self::DerivedSuperNewTarget(_)
                | Self::DerivedSuperCompletion(_)
                | Self::ForOfIterator(_)
                | Self::ForOfNextMethod(_)
                | Self::ForOfCatch(_)
                | Self::ForOfDisabledCatch(_)
                | Self::ForOfExhaustedIterator(_)
                | Self::ForOfExhaustedNextMethod(_)
                | Self::ForOfExhaustedCatch(_)
                | Self::ForOfClosableIterator(_)
                | Self::ForOfClosableNextMethod(_)
                | Self::ForOfClosableCatch(_)
                | Self::ForOfCloseIterator(_)
                | Self::ForOfCloseNextMethod(_)
                | Self::ForOfCloseDummy(_)
                | Self::YieldStarIterator(_)
                | Self::YieldStarNextMethod(_)
                | Self::YieldStarDummy(_)
                | Self::CatchMarker { .. }
                | Self::FinallyPending { .. }
                | Self::FinallyReturn { .. }
        )
    }

    const fn is_catch_value(self) -> bool {
        matches!(self, Self::CatchMarker { .. } | Self::CatchException(_))
    }

    const fn is_finally_value(self) -> bool {
        matches!(
            self,
            Self::FinallyPending { .. } | Self::FinallyReturn { .. }
        )
    }

    const fn is_for_of_value(self) -> bool {
        matches!(
            self,
            Self::ForOfIterator(_)
                | Self::ForOfNextMethod(_)
                | Self::ForOfCatch(_)
                | Self::ForOfDisabledCatch(_)
                | Self::ForOfAwaitResult(_)
                | Self::ForOfAwaitedResult(_)
                | Self::ForOfExhaustedIterator(_)
                | Self::ForOfExhaustedNextMethod(_)
                | Self::ForOfExhaustedCatch(_)
                | Self::ForOfClosableIterator(_)
                | Self::ForOfClosableNextMethod(_)
                | Self::ForOfClosableCatch(_)
                | Self::ForOfValue(_)
                | Self::ForOfDone(_)
                | Self::ForOfHeadValue(_)
                | Self::ForOfReturnValue(_)
                | Self::ForOfCloseIterator(_)
                | Self::ForOfCloseNextMethod(_)
                | Self::ForOfCloseDummy(_)
                | Self::YieldStarIterator(_)
                | Self::YieldStarNextMethod(_)
                | Self::YieldStarDummy(_)
                | Self::YieldStarIteratorResult(_)
                | Self::YieldStarDone(_)
                | Self::YieldStarYieldResult(_)
                | Self::YieldStarYieldValue(_)
                | Self::YieldStarFinalResult(_)
                | Self::YieldStarResumeValue(_)
                | Self::YieldStarResumeMode(_)
                | Self::YieldStarResumeModeTest(_)
                | Self::YieldStarIsThrow(_)
                | Self::YieldStarCallValue(_, _)
                | Self::YieldStarMethodMissing(_, _)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CertifiedIterationLocalPut {
    local: u32,
    cursor_site: BytecodePc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CertifiedCatchLocalPut {
    local: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CertifiedNipCatchTransform {
    input_depth: u32,
    retained_prefix: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct InternalStackCertificate {
    iteration_local_puts: Vec<Option<CertifiedIterationLocalPut>>,
    catch_local_puts: Vec<Option<CertifiedCatchLocalPut>>,
    nip_catch_transforms: Vec<Option<CertifiedNipCatchTransform>>,
    finally_continuations: Vec<Vec<InstructionIndex>>,
    ret_finalizers: Vec<Option<InstructionIndex>>,
}

impl InternalStackCertificate {
    fn certifies_iteration_local_put(&self, instruction: usize, local: u32) -> bool {
        self.iteration_local_puts
            .get(instruction)
            .copied()
            .flatten()
            .is_some_and(|certificate| certificate.local == local)
    }

    fn certifies_catch_local_put(&self, instruction: usize, local: u32) -> bool {
        self.catch_local_puts
            .get(instruction)
            .copied()
            .flatten()
            .is_some_and(|certificate| certificate.local == local)
    }

    fn nip_catch_transform(&self, instruction: usize) -> Option<CertifiedNipCatchTransform> {
        self.nip_catch_transforms
            .get(instruction)
            .copied()
            .flatten()
    }

    fn effective_successors<'a>(
        &'a self,
        instructions: &'a [VerifiedInstruction],
        instruction: usize,
    ) -> EffectiveSuccessors<'a> {
        let ret_finalizer = self.ret_finalizers.get(instruction).copied().flatten();
        effective_successors(
            instructions,
            instruction,
            &self.finally_continuations,
            ret_finalizer,
        )
    }

    fn has_effective_successor(
        &self,
        instructions: &[VerifiedInstruction],
        instruction: usize,
        target: u32,
    ) -> bool {
        self.effective_successors(instructions, instruction)
            .any(|edge| edge.target.get() == target)
    }

    fn is_finally_target(&self, instruction: usize) -> bool {
        self.finally_continuations
            .get(instruction)
            .is_some_and(|continuations| !continuations.is_empty())
    }
}

#[derive(Clone, Copy, Default)]
struct IterationLocalPutSummary {
    unchecked_puts: u32,
    certified_puts: u32,
    cursor_site: Option<BytecodePc>,
    first_certified_pc: Option<BytecodePc>,
    has_uncertified_put: bool,
    multiple_cursor_sites: bool,
    declarative_authority: bool,
}

#[derive(Clone, Copy)]
struct InternalStackTransfer {
    normal_completion: bool,
    iteration_branch_value: Option<IterationBranchValue>,
    ret_finalizer: Option<InstructionIndex>,
}

#[derive(Clone, Copy)]
enum IterationBranchValue {
    ForIn(BytecodePc),
    ForOf {
        site: BytecodePc,
        extras: usize,
    },
    YieldStarDone {
        site: BytecodePc,
        branch_when_true: bool,
    },
    YieldStarMethod {
        site: BytecodePc,
        kind: YieldStarCallKind,
    },
}

#[derive(Clone, Copy)]
struct EffectiveEdge {
    target: InstructionIndex,
    is_branch_target: bool,
    enters_finally: bool,
}

enum EffectiveSuccessors<'a> {
    Structural {
        edges: [Option<EffectiveEdge>; 3],
        next: usize,
    },
    One(Option<EffectiveEdge>),
    Ret(std::slice::Iter<'a, InstructionIndex>),
}

impl Iterator for EffectiveSuccessors<'_> {
    type Item = EffectiveEdge;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Structural { edges, next } => {
                while *next < edges.len() {
                    let edge = edges[*next].take();
                    *next += 1;
                    if edge.is_some() {
                        return edge;
                    }
                }
                None
            }
            Self::One(edge) => edge.take(),
            Self::Ret(continuations) => continuations.next().copied().map(|target| EffectiveEdge {
                target,
                is_branch_target: false,
                enters_finally: false,
            }),
        }
    }
}

fn effective_successors<'a>(
    instructions: &'a [VerifiedInstruction],
    instruction: usize,
    finally_continuations: &'a [Vec<InstructionIndex>],
    ret_finalizer: Option<InstructionIndex>,
) -> EffectiveSuccessors<'a> {
    let Some(verified) = instructions.get(instruction) else {
        return EffectiveSuccessors::Structural {
            edges: [None; 3],
            next: 0,
        };
    };
    let successors = verified.successors();
    match verified.decoded().instruction().opcode() {
        FinalOpcode::Gosub => {
            EffectiveSuccessors::One(successors.branch_target().map(|target| EffectiveEdge {
                target,
                is_branch_target: false,
                enters_finally: true,
            }))
        }
        FinalOpcode::Ret => {
            let continuations = ret_finalizer
                .and_then(|target| finally_continuations.get(target.get() as usize))
                .map_or([].as_slice(), Vec::as_slice);
            EffectiveSuccessors::Ret(continuations.iter())
        }
        _ => EffectiveSuccessors::Structural {
            edges: [
                successors.fallthrough().map(|target| EffectiveEdge {
                    target,
                    is_branch_target: false,
                    enters_finally: false,
                }),
                successors.branch_target().map(|target| EffectiveEdge {
                    target,
                    is_branch_target: true,
                    enters_finally: false,
                }),
                successors.jump_target().map(|target| EffectiveEdge {
                    target,
                    is_branch_target: false,
                    enters_finally: false,
                }),
            ],
            next: 0,
        },
    }
}
