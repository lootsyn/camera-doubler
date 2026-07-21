from __future__ import annotations

import json

from .builder import assert_lerobot_version


if __name__ == "__main__":
    print(json.dumps({"ready": True, "lerobot_version": assert_lerobot_version()}))
