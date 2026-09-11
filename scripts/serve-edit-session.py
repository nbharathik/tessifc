# SPDX-License-Identifier: Apache-2.0
"""Serve the viewer and one IFC file as a live editing session.

The implementation lives in the optional IfcOpenShell adapter package; this
launcher only puts it on the path so the documented command keeps working.
"""

from __future__ import annotations

import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "adapters" / "ifcopenshell"))

from tessifc_session.cli import main  # noqa: E402
from tessifc_session.server import create_server  # noqa: E402,F401
from tessifc_session.session import EditSession as FileSession  # noqa: E402,F401

if __name__ == "__main__":
    main()
