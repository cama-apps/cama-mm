//! Transport-independent atmospheric flame policy for Dig.
//!
//! Python's `services.dig_flame` deliberately has a very small contract: pick
//! one canonical catastrophic line, format a post as `prefix *line*`, and
//! schedule the send without allowing a Discord or event-loop failure to
//! escape into the Dig interaction.  This file keeps that policy independent
//! of Serenity and Tokio.  A runtime adapter supplies [`AtmosphericChannel`]
//! and [`AtmosphericTaskSpawner`] when the module is admitted.

use std::future::Future;
use std::pin::Pin;

pub const CATASTROPHIC_PREFIX: &str = "💥";

pub const CATASTROPHIC_LINES: [&str; 8] = [
    "Far below, a tunnel folds shut. Stone settles for a long time.",
    "A great groan rolls through the rock. Somewhere in the dark, a miner is buried.",
    "Beams snap. Lanterns wink out. The earth swallows a tunnel whole.",
    "The deep gives a sound like a closing door. Then silence.",
    "Dust climbs out of a shaft that no longer goes anywhere.",
    "A pickaxe rings once, then stops. The ceiling has come for it.",
    "The hill shifts. Birds lift from leagues away.",
    "Below the bones of the world, a chamber that was now isn't.",
];

/// The narrow entropy seam needed by the canonical line picker.
pub trait FlameEntropy {
    /// Return an index in `0..upper_exclusive`.
    fn index(&mut self, upper_exclusive: usize) -> usize;
}

impl<T: FlameEntropy + ?Sized> FlameEntropy for &mut T {
    fn index(&mut self, upper_exclusive: usize) -> usize {
        T::index(self, upper_exclusive)
    }
}

/// Small deterministic entropy source useful for policy tests and retry-safe
/// event adapters.  Production can instead implement [`FlameEntropy`] over
/// the application's shared RNG.
#[derive(Clone, Debug)]
pub struct SplitMix64Entropy {
    state: u64,
}

impl SplitMix64Entropy {
    #[must_use]
    pub const fn seeded(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }
}

impl FlameEntropy for SplitMix64Entropy {
    fn index(&mut self, upper_exclusive: usize) -> usize {
        assert!(upper_exclusive > 0, "line pool must not be empty");
        (self.next_u64() % upper_exclusive as u64) as usize
    }
}

#[must_use]
pub fn pick_catastrophic_line<E: FlameEntropy + ?Sized>(entropy: &mut E) -> &'static str {
    CATASTROPHIC_LINES[entropy.index(CATASTROPHIC_LINES.len())]
}

/// Failure returned by a channel when it cannot create a send future.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlameSendError;

/// Failure returned by a scheduler when no task can be detached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlameTaskError;

pub type FlameSendFuture =
    Pin<Box<dyn Future<Output = Result<(), FlameSendError>> + Send + 'static>>;

/// Minimal Discord-channel seam.  `Ok(None)` represents a channel-like
/// object without `send`, while `Err` represents a synchronous send failure.
pub trait AtmosphericChannel {
    fn send(&self, content: String) -> Result<Option<FlameSendFuture>, FlameSendError>;
}

/// Minimal event-loop seam.  A live runtime detaches the future; a missing
/// loop is represented by `None` at the call site and drops it just as the
/// Python helper drops a coroutine when `asyncio.create_task` has no loop.
pub trait AtmosphericTaskSpawner {
    fn spawn(&self, task: FlameSendFuture) -> Result<(), FlameTaskError>;
}

/// Format and fire-and-forget one atmospheric post.
pub fn post_atmospheric<C, S>(channel: Option<&C>, spawner: Option<&S>, prefix: &str, line: &str)
where
    C: AtmosphericChannel,
    S: AtmosphericTaskSpawner,
{
    if channel.is_none() || line.is_empty() {
        return;
    }

    let formatted = format!("{prefix} *{}*", line.trim());
    let Some(channel) = channel else {
        return;
    };
    let Ok(Some(task)) = channel.send(formatted) else {
        return;
    };

    let Some(spawner) = spawner else {
        // No running loop: deliberately drop the detached future.
        return;
    };
    let _ = spawner.spawn(task);
}

/// Pick and post one canonical catastrophic cave-in line.
pub fn post_catastrophic<C, S, E>(channel: Option<&C>, spawner: Option<&S>, entropy: &mut E)
where
    C: AtmosphericChannel,
    S: AtmosphericTaskSpawner,
    E: FlameEntropy + ?Sized,
{
    post_atmospheric(
        channel,
        spawner,
        CATASTROPHIC_PREFIX,
        pick_catastrophic_line(entropy),
    );
}

#[cfg(test)]
#[path = "dig_flame_policy_tests.rs"]
mod tests;
