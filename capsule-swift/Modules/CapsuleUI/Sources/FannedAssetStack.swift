import AssetKit
import ImagePipeline
import SwiftUI

// MARK: - FannedAssetStack

/// Up to three photos, splayed like a hand of cards.
///
/// The preview a map pin shows: enough to recognise the place without leaving
/// the map. Three is the limit because a fourth card is hidden behind the third
/// at any rotation small enough to still read as a stack.
///
/// **Newest on top and upright.** The fan rotates the cards *behind* the first
/// rather than rotating all of them around a shared centre, so the photo the
/// user is actually looking at is never tilted — the tilt is what says "there
/// are more", and it should cost the top card nothing.
public struct FannedAssetStack: View {
    /// The most a fan shows, however many the place holds.
    public static let maximumCards = 3

    private let assets: [Asset]
    private let placeholderCount: Int
    private let side: CGFloat
    private let thumbnails: any ThumbnailProvider

    /// - Parameter placeholderCount: how many blank cards to fan while the
    ///   photos are still being fetched. A fan that appears only once its
    ///   contents arrive reads, at the moment of the tap, as nothing having
    ///   happened.
    public init(
        assets: [Asset],
        placeholderCount: Int = 0,
        side: CGFloat,
        thumbnails: any ThumbnailProvider
    ) {
        self.assets = Array(assets.prefix(Self.maximumCards))
        self.placeholderCount = min(placeholderCount, Self.maximumCards)
        self.side = side
        self.thumbnails = thumbnails
    }

    /// How many cards are drawn: the photos, or the placeholders standing in
    /// for them.
    private var cardCount: Int { max(assets.count, assets.isEmpty ? placeholderCount : 0) }

    public var body: some View {
        ZStack {
            // Reversed so the first card is drawn last and lands on top.
            ForEach((0 ..< cardCount).reversed(), id: \.self) { index in
                card(at: index)
                    .rotationEffect(.degrees(angle(for: index)), anchor: .bottom)
                    .offset(x: offset(for: index))
                    .zIndex(Double(cardCount - index))
            }
        }
        // The fan is one preview, not three photos to sweep through.
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("ios.places.preview.accessibility \(cardCount)"))
    }

    @ViewBuilder
    private func card(at index: Int) -> some View {
        if index < assets.count {
            AssetThumbnailCard(asset: assets[index], side: side, thumbnails: thumbnails)
        } else {
            AssetThumbnailCard.placeholder(side: side)
        }
    }

    /// Alternating tilt, growing with depth: the second card leans right, the
    /// third further left, so both stay visible instead of stacking up on one
    /// side and hiding each other.
    private func angle(for index: Int) -> Double {
        switch index {
        case 0: 0
        case 1: 7
        default: -7
        }
    }

    private func offset(for index: Int) -> CGFloat {
        switch index {
        case 0: 0
        case 1: side * 0.10
        default: -side * 0.10
        }
    }
}
