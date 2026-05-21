use crate::domain::{DetectionMethod, MemberRole, StackType};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackHint {
    pub detection_key: String,
    pub detection_method: DetectionMethod,
    pub member_role: MemberRole,
    pub stack_type: StackType,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DetectionMethod, MemberRole, StackType};

    fn cbor_roundtrip<
        T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
    >(
        val: &T,
    ) -> T {
        let mut buf = vec![];
        ciborium::ser::into_writer(val, &mut buf).unwrap();
        ciborium::de::from_reader(buf.as_slice()).unwrap()
    }

    #[test]
    fn test_round_trip() {
        let hint = StackHint {
            detection_key: "img_1234".to_string(),
            detection_method: DetectionMethod::FilenameStem,
            member_role: MemberRole::Primary,
            stack_type: StackType::RawJpeg,
        };
        assert_eq!(hint, cbor_roundtrip(&hint));
    }
}
