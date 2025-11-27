use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GitHubContent {
    pub name: String,
}
