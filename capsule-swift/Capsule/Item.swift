//
//  Item.swift
//  Capsule
//
//  Created by Justin Chung on 3/29/25.
//

import Foundation
import SwiftData

// TODO: Remove this
@Model
final class Item {
    var timestamp: Date

    init(timestamp: Date) {
        self.timestamp = timestamp
    }
}
