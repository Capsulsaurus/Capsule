use serde::{Deserialize, Serialize};

/// Generic GPS location.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GpsLocation {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f32>,
}

// TODO: Add conversion from WGS-84 to GCJ-02
