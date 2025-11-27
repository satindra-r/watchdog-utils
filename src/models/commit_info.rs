use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CommitInfo {
    pub sha: String,
}
