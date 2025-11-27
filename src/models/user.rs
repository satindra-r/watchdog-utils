use serde::Deserialize;

#[derive(Deserialize)]
pub struct GroupRequest {
    pub user: String,
    pub group: String,
}
