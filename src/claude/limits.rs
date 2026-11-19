//! Usage limits shared by subagent providers, saved as one reading per group of models.

use crate::store::AccountUsage;

/// One group of models that share limits, by the name the provider gives it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pool {
    pub name: String,
    pub usage: AccountUsage,
}
