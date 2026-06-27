/**
 * A photo or video as the web client renders it.
 *
 * This is the thin DISPLAY shape the UI needs — deliberately not a mirror of
 * capsule-core's full asset/sidecar model (provenance chain, closed enums, MLS
 * state). When a real data source is wired, the server/SDK adapter maps the
 * high-level capsule-core / capsule-sdk model down to this type. Each field
 * notes its eventual source.
 */
export interface Asset {
    /** Stable asset id (capsule-core: `Asset.id`). */
    id: string;
    /** Full-resolution media URL (server: original-tier media endpoint). */
    url: string;
    /** Low-res thumbnail URL (server: tiered thumbnail endpoint). */
    thumbnailUrl: string;
    /** Capture/creation time (capsule-core: sidecar `created_at`). */
    date: Date;
    /** Media kind (capsule-core: `content_type`, narrowed for display). */
    type: 'image' | 'video';
    /** Human-readable duration for videos, e.g. "0:15". */
    duration?: string;
    /** Optional place label (capsule-core: reverse-geocoded, privacy-stripped). */
    location?: string;
    /** Intrinsic pixel width, for justified layout. */
    width: number;
    /** Intrinsic pixel height, for justified layout. */
    height: number;
    /** ThumbHash (base64) for an instant blurred placeholder. */
    thumbhash: string;
}
