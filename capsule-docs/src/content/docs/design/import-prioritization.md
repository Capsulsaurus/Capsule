---
title: Import Prioritization
description: How to prioritize imports for best user experience for large collections.
---

## Import & Upload Prioritization Criterias

- **File Size:** Smaller files might be processed first to give a quicker sense of progress, or larger files might be prioritized if they are deemed more critical.
  - While file size is a useful heuristic, for internal ordering, we should let the order files are uploaded be naturally determined by simultaneous uploads and the network conditions, which would fall to the responsibility of the underlying file transfer protocol (i.e., as of writing, )
- **Last Modified Times:** Newer or recently modified files might be more relevant to the user. (Note this filesystem metadata may not be always reliable so some fallbacks may be needed. Last accessed time was also considered but relatime makes this heuristic relatively noisy.)
- **Directory Depth:** Files closer to the root of the specified paths might be processed first.

### Non-Criterias

- **File Type/Extension:** Prioritizing purely by file types may result in anomalies. Instead we should have exceptions for certain sidecar files (e.g. `.xmp` associated with an image, or `.wav` associated with a video file).
