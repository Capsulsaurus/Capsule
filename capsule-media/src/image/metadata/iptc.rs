use serde::{Deserialize, Serialize};

// TODO: Complete all the tags in the spec.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IptcData {
    // ApplicationRecord tags
    pub byline: Option<Vec<String>>,
    pub byline_title: Option<Vec<String>>,
    pub caption: Option<String>,
    pub copyright_notice: Option<String>,
    pub keywords: Vec<String>,
    pub city: Option<String>,
    pub province_state: Option<String>,
    pub country_name: Option<String>,
    pub source: Option<String>,
}
