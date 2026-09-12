#!/usr/bin/env bash
set -eu -o pipefail

(mkdir all-filters && cd all-filters
  cat <<EOF > .gitattributes
* ident text=auto eol=crlf working-tree-encoding=ISO-8859-1 filter=arrow
EOF
)

(mkdir no-filters && cd no-filters
  touch .gitattributes
)

(mkdir unknown-encoding && cd unknown-encoding
  cat <<EOF > .gitattributes
* text eol=crlf working-tree-encoding=not-an-encoding
EOF
)

(mkdir driver-only && cd driver-only
  cat <<EOF > .gitattributes
* filter=arrow
EOF
)
