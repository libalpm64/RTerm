use super::*;

impl Activity {
    pub(in crate::app) fn saved(&self) -> &VecDeque<AgentAuditEntry> {
        &self.saved
    }
}
