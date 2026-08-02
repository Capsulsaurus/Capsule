# Legacy Review

This directory preserves implementation material that may inform a rewrite but is not supported,
built, formatted, linted, packaged, or deployed. Cargo manifests are deliberately disabled so
these sources cannot become implicit workspace members.

Code may leave this directory only after its replacement contract and tests exist. Copying a file
back into active code without reviewing its security assumptions, framework coupling, logging,
error handling, and internationalization is prohibited.
