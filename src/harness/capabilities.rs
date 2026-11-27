//! What an adapter lets m4code do with its agent. The core asks here before every action that reaches the agent, and
//! never names a harness itself: a refused action stays visible but disabled, with the adapter's reason as its
//! tooltip, and the same answer refuses it when a tool call or an automatic step asks for it.

/// Whether one action is allowed, and why not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Allowed,
    /// Shown to the user as is, so it reads as a whole sentence.
    Refused(&'static str),
}

impl Capability {
    pub fn allowed(self) -> bool {
        self == Self::Allowed
    }

    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Allowed => None,
            Self::Refused(reason) => Some(reason),
        }
    }
}

/// Who may move a conversation onto, or off, a harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff {
    /// The user, the agent (`set_model`) and auto-handoff on a usage limit.
    Any,
    Never(&'static str),
}

/// Who asks for a handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Initiator {
    User,
    /// The agent through `set_model`, or m4code itself, as auto-handoff on a usage limit does.
    Automatic,
}

impl Handoff {
    /// `by` is part of the question so a rule that tells the user from the agent needs no change at the call sites.
    pub fn allows(self, _by: Initiator) -> Capability {
        match self {
            Self::Any => Capability::Allowed,
            Self::Never(reason) => Capability::Refused(reason),
        }
    }
}

/// The actions an adapter allows. Every field is one button or automatic step in the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Rewind (and edit & rewind) to an earlier message, which starts a new session seeded with the conversation
    /// before it.
    pub rewind: Capability,
    /// Fork a session from a message into a new one seeded with the conversation up to it.
    pub fork: Capability,
    /// Continue in a fresh linked session, seeded with a summary of this one.
    pub continue_fresh: Capability,
    /// m4code's own compaction for a session whose agent doesn't list its compact command: a summary, then a new
    /// session seeded with it.
    pub compaction_fallback: Capability,
    /// Answering m4code's own prompts: session titles, commit messages and summaries.
    pub text_generation: Capability,
    /// Moving a conversation from another harness onto this one.
    pub handoff_in: Handoff,
    /// Moving a conversation from this harness onto another one.
    pub handoff_out: Handoff,
    /// Running as a subagent that another session started.
    pub subagent: Capability,
    /// Its sessions starting subagents.
    pub spawns_subagents: Capability,
}

impl Capabilities {
    pub const ALL: Self = Self {
        rewind: Capability::Allowed,
        fork: Capability::Allowed,
        continue_fresh: Capability::Allowed,
        compaction_fallback: Capability::Allowed,
        text_generation: Capability::Allowed,
        handoff_in: Handoff::Any,
        handoff_out: Handoff::Any,
        subagent: Capability::Allowed,
        spawns_subagents: Capability::Allowed,
    };

    /// For an adapter this build leaves out.
    pub const NONE: Self = {
        const GONE: &str = "This adapter is unavailable in this build.";
        Self {
            rewind: Capability::Refused(GONE),
            fork: Capability::Refused(GONE),
            continue_fresh: Capability::Refused(GONE),
            compaction_fallback: Capability::Refused(GONE),
            text_generation: Capability::Refused(GONE),
            handoff_in: Handoff::Never(GONE),
            handoff_out: Handoff::Never(GONE),
            subagent: Capability::Refused(GONE),
            spawns_subagents: Capability::Refused(GONE),
        }
    };
}

/// Whether a conversation may move from `from` to `to`: the first refusal, the source's before the target's.
pub fn handoff(from: super::Harness, to: super::Harness, by: Initiator) -> Capability {
    if from == to {
        return Capability::Allowed;
    }
    let out = from.adapter().capabilities.handoff_out.allows(by);
    if out.allowed() { to.adapter().capabilities.handoff_in.allows(by) } else { out }
}

/// Whether a session on `parent` may start a subagent on `child`. A parent that isn't an agent session asks only the
/// child.
pub fn subagent(parent: impl Into<Option<super::Harness>>, child: super::Harness) -> Capability {
    let out = parent.into().map_or(Capability::Allowed, |parent| parent.adapter().capabilities.spawns_subagents);
    if out.allowed() { child.adapter().capabilities.subagent } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_handoff_gives_its_reason_to_whoever_asks() {
        for by in [Initiator::User, Initiator::Automatic] {
            assert_eq!(Handoff::Never("No.").allows(by).refusal(), Some("No."));
            assert!(Handoff::Any.allows(by).allowed());
        }
    }

    #[test]
    fn staying_on_one_harness_is_never_a_handoff() {
        for harness in super::super::Harness::ALL {
            assert!(handoff(harness, harness, Initiator::Automatic).allowed());
        }
    }
}
