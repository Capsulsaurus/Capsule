import CapsuleFoundation
import Foundation
import MetricKit

extension CrashSummary {
    /// Map a MetricKit crash diagnostic into a PII-free summary.
    init(from crash: MXCrashDiagnostic, at time: Date) {
        self.init(
            kind: .crash,
            exceptionType: crash.exceptionType?.intValue,
            exceptionCode: crash.exceptionCode?.intValue,
            signal: crash.signal?.intValue,
            terminationReason: crash.terminationReason,
            osVersion: crash.metaData.osVersion,
            appBuild: crash.metaData.applicationBuildVersion,
            timestamp: time
        )
    }
}
