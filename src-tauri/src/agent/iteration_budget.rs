#[derive(Debug, Clone)]
pub(super) struct IterationBudget {
    max_total: u32,
    used: u32,
}

impl IterationBudget {
    pub(super) fn new(max_total: u32) -> Self {
        Self {
            max_total: max_total.max(1),
            used: 0,
        }
    }

    pub(super) fn consume(&mut self) -> bool {
        if self.used >= self.max_total {
            return false;
        }
        self.used += 1;
        true
    }

    pub(super) fn refund(&mut self) {
        if self.used > 0 {
            self.used -= 1;
        }
    }

    pub(super) fn max_total(&self) -> u32 {
        self.max_total
    }

    pub(super) fn used(&self) -> u32 {
        self.used
    }

    pub(super) fn remaining(&self) -> u32 {
        self.max_total.saturating_sub(self.used)
    }

    pub(super) fn exhausted(&self) -> bool {
        self.remaining() == 0
    }
}
