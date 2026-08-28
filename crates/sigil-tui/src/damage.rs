//! Render invalidation produced by normalized input and host/application updates.
//!
//! A damage value is deliberately small and copyable. The launcher can merge damage while it
//! drains one input batch, then perform at most one terminal present for that batch.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Damage(u8);

impl Damage {
    pub(crate) const NONE: Self = Self(0);
    pub(crate) const INPUT: Self = Self(1 << 0);
    pub(crate) const HOST_EFFECT: Self = Self(1 << 1);
    pub(crate) const ASYNC: Self = Self(1 << 2);
    pub(crate) const TERMINAL: Self = Self(1 << 3);
    #[cfg(test)]
    pub(crate) const FULL: Self =
        Self(Self::INPUT.0 | Self::HOST_EFFECT.0 | Self::ASYNC.0 | Self::TERMINAL.0);

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::Damage;

    #[test]
    fn damage_union_is_idempotent_and_preserves_all_causes() {
        let merged = Damage::INPUT
            .union(Damage::HOST_EFFECT)
            .union(Damage::ASYNC);
        assert!(!merged.is_empty());
        assert_eq!(merged.union(Damage::INPUT), merged);
        assert_eq!(Damage::NONE.union(merged), merged);
    }
}
