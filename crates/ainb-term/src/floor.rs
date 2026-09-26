//! Driver floor: who may type into, and size, one pane (D16, spec R2 row).
//!
//! The floor is keyed per STREAM, not per principal (RECONCILED T17): two
//! browser tabs share one operator principal and still arbitrate. Exactly
//! one stream holds it or nobody does. Every change of holder bumps
//! `floor_gen`, so a client that types with the generation it last saw is
//! refused the moment the floor moved (RECONCILED M8, M9, M11). The resize
//! owner is always the holder: the sizer control client lives and dies with
//! the floor (R2-plan S10, DV2).
//!
//! ```text
//!            acquire / take / input{gen: none}
//!   free ───────────────────────────────────▶ held by S   (gen + 1)
//!    ▲                                           │
//!    └────── release(S) / detach(S) / close ─────┘          (gen + 1)
//!                     take(T), T != S ──▶ held by T          (gen + 1)
//! ```
//!
//! Detach, socket close and a 4403 revoke all release the floor at once, with
//! no grace window (R2-plan release rule); they all route through
//! [`Floor::release`]. Native tmux clients are not arbitrated here at all
//! (spec :258): presence only reports them.

/// A stream id, unique per connection while attached.
pub type StreamId = u64;

/// Who holds the floor. Mirrors the frozen wire `FloorHolder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The stream holding the floor.
    pub stream_id: StreamId,
    /// The ledger principal (`local` or `device:<id>`).
    pub principal: String,
    /// The registry display name for a device, the surface kind for a local
    /// connection. Never the name a client declared in hello.
    pub label: String,
}

/// A refused floor operation: the holder and generation at refusal time,
/// which the daemon puts in `error.data` beside `floor_denied`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    /// The holder at refusal, or none when the floor was free and the
    /// caller's generation was stale.
    pub holder: Option<Holder>,
    /// The generation at refusal.
    pub floor_gen: u64,
}

/// The floor of one pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Floor {
    holder: Option<Holder>,
    floor_gen: u64,
}

impl Floor {
    /// A free floor at generation 0.
    pub const fn new() -> Self {
        Self {
            holder: None,
            floor_gen: 0,
        }
    }

    /// The holder, or none when free.
    pub const fn holder(&self) -> Option<&Holder> {
        self.holder.as_ref()
    }

    /// The current generation.
    pub const fn floor_gen(&self) -> u64 {
        self.floor_gen
    }

    /// Whether this stream holds the floor.
    pub fn is_holder(&self, stream_id: StreamId) -> bool {
        self.holder.as_ref().is_some_and(|h| h.stream_id == stream_id)
    }

    /// The stream that may resize the pane: the holder, or nobody.
    pub fn resize_owner(&self) -> Option<StreamId> {
        self.holder.as_ref().map(|h| h.stream_id)
    }

    fn denied(&self) -> Denied {
        Denied {
            holder: self.holder.clone(),
            floor_gen: self.floor_gen,
        }
    }

    fn set_holder(&mut self, holder: Option<Holder>) -> u64 {
        self.holder = holder;
        self.floor_gen += 1;
        self.floor_gen
    }

    /// Take the floor if free. Holding it already is a success without a
    /// bump. Returns the generation the caller now holds.
    pub fn acquire(&mut self, who: Holder) -> Result<u64, Denied> {
        match &self.holder {
            None => Ok(self.set_holder(Some(who))),
            Some(h) if h.stream_id == who.stream_id => Ok(self.floor_gen),
            Some(_) => Err(self.denied()),
        }
    }

    /// Take the floor from whoever holds it. Holding it already is a no-op.
    pub fn take(&mut self, who: Holder) -> u64 {
        if self.is_holder(who.stream_id) {
            self.floor_gen
        } else {
            self.set_holder(Some(who))
        }
    }

    /// Give the floor up. Only the holder can; anyone else is a no-op.
    /// Returns `true` when the floor changed. This is also what detach,
    /// socket close and revoke call.
    pub fn release(&mut self, stream_id: StreamId) -> bool {
        if self.is_holder(stream_id) {
            self.set_holder(None);
            true
        } else {
            false
        }
    }

    /// Whether `who` may type now. With a generation, `who` must hold the
    /// floor at exactly that generation. Without one, "acquire if free":
    /// a holder types on, a free floor is taken, anyone else is refused.
    /// Returns the generation the input was accepted under.
    pub fn input(&mut self, who: Holder, floor_gen: Option<u64>) -> Result<u64, Denied> {
        match floor_gen {
            Some(gen) => {
                if self.is_holder(who.stream_id) && gen == self.floor_gen {
                    Ok(gen)
                } else {
                    Err(self.denied())
                }
            }
            None => self.acquire(who),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn who(stream_id: StreamId) -> Holder {
        Holder {
            stream_id,
            principal: if stream_id.is_multiple_of(2) {
                "local".to_string()
            } else {
                "device:01K5A0000000000000000ABCDE".to_string()
            },
            label: format!("viewer {stream_id}"),
        }
    }

    #[test]
    fn acquire_release_and_take_bump_the_generation_only_on_change() {
        let mut f = Floor::new();
        assert_eq!(f.floor_gen(), 0);
        assert_eq!(f.resize_owner(), None);
        assert_eq!(f.acquire(who(1)), Ok(1));
        assert_eq!(
            f.acquire(who(1)),
            Ok(1),
            "re-acquire by the holder: no bump"
        );
        assert_eq!(f.resize_owner(), Some(1));
        let denied = f.acquire(who(2)).unwrap_err();
        assert_eq!(denied.holder.as_ref().map(|h| h.stream_id), Some(1));
        assert_eq!(denied.floor_gen, 1);
        assert!(!f.release(2), "a non-holder cannot release");
        assert_eq!(f.floor_gen(), 1);
        assert_eq!(f.take(who(2)), 2);
        assert_eq!(f.take(who(2)), 2, "take by the holder: no bump");
        assert!(f.is_holder(2));
        assert!(f.release(2));
        assert_eq!(f.floor_gen(), 3);
        assert_eq!(f.holder(), None);
        assert_eq!(f.take(who(1)), 4, "take on a free floor acquires");
    }

    #[test]
    fn input_follows_the_generation_rule() {
        let mut f = Floor::new();
        // No generation on a free floor: acquire.
        assert_eq!(f.input(who(1), None), Ok(1));
        assert_eq!(f.input(who(1), Some(1)), Ok(1));
        assert_eq!(
            f.input(who(1), None),
            Ok(1),
            "holder without a gen types on"
        );
        // The other stream: refused either way.
        assert_eq!(
            f.input(who(2), None),
            Err(Denied {
                holder: Some(who(1)),
                floor_gen: 1
            })
        );
        assert!(f.input(who(2), Some(1)).is_err(), "right gen, wrong stream");
        // The floor moves: the old generation is refused even for the taker.
        assert_eq!(f.take(who(2)), 2);
        assert!(f.input(who(2), Some(1)).is_err(), "stale gen");
        assert_eq!(f.input(who(2), Some(2)), Ok(2));
        assert!(f.input(who(1), Some(2)).is_err());
        // Released: a stale gen on a free floor is refused with no holder.
        assert!(f.release(2));
        assert_eq!(
            f.input(who(2), Some(2)),
            Err(Denied {
                holder: None,
                floor_gen: 3
            })
        );
        assert_eq!(f.input(who(2), None), Ok(4));
    }

    /// The daemon's stand-in for two viewers driving one floor, with the
    /// generation each of them last saw: what a real client sends back.
    #[derive(Debug, Clone)]
    enum Op {
        Acquire(u8),
        Take(u8),
        Release(u8),
        Detach(u8),
        InputSeen(u8),
        InputFresh(u8),
    }

    fn op() -> impl Strategy<Value = Op> {
        let v = 0..2u8;
        prop_oneof![
            v.clone().prop_map(Op::Acquire),
            v.clone().prop_map(Op::Take),
            v.clone().prop_map(Op::Release),
            v.clone().prop_map(Op::Detach),
            v.clone().prop_map(Op::InputSeen),
            v.prop_map(Op::InputFresh),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]
        #[test]
        fn two_viewers_never_type_out_of_turn(ops in prop::collection::vec(op(), 1..64)) {
            let mut f = Floor::new();
            let mut seen = [0u64; 2];
            let mut last_gen = f.floor_gen();
            for o in ops {
                let holder_before = f.resize_owner();
                let (viewer, accepted) = match o {
                    Op::Acquire(v) => {
                        let r = f.acquire(who(u64::from(v)));
                        if let Ok(g) = r { seen[v as usize] = g; }
                        (v, r.is_ok())
                    }
                    Op::Take(v) => {
                        seen[v as usize] = f.take(who(u64::from(v)));
                        (v, true)
                    }
                    Op::Release(v) | Op::Detach(v) => {
                        f.release(u64::from(v));
                        prop_assert!(!f.is_holder(u64::from(v)), "released stream is never the holder");
                        (v, false)
                    }
                    Op::InputSeen(v) => {
                        let r = f.input(who(u64::from(v)), Some(seen[v as usize]));
                        (v, r.is_ok())
                    }
                    Op::InputFresh(v) => {
                        let r = f.input(who(u64::from(v)), None);
                        if let Ok(g) = r { seen[v as usize] = g; }
                        (v, r.is_ok())
                    }
                };
                // Accepted input or floor comes only from the holder.
                if accepted {
                    prop_assert!(f.is_holder(u64::from(viewer)), "accepted for a non-holder");
                }
                // Input with a generation is accepted only when nobody took
                // the floor in between: the holder is unchanged.
                if matches!(o, Op::InputSeen(_)) && accepted {
                    prop_assert_eq!(holder_before, Some(u64::from(viewer)));
                }
                // The generation is monotonic and moves exactly with the holder.
                let g = f.floor_gen();
                prop_assert!(g >= last_gen, "generation went backwards");
                if f.resize_owner() != holder_before {
                    prop_assert_eq!(g, last_gen + 1, "holder changed without a single bump");
                } else {
                    prop_assert_eq!(g, last_gen, "bump without a holder change");
                }
                last_gen = g;
                // The resize owner is the holder or nobody.
                prop_assert_eq!(f.resize_owner(), f.holder().map(|h| h.stream_id));
            }
        }
    }
}
