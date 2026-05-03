"""Phase 31.4 — Event payload mirroring the Rust SDK's `Event` shape.

A single dataclass that the dispatch loop deserializes inbound
notifications into and that handlers serialize outbound publishes
from. Topic + source + payload are required; correlation_id and
metadata are optional.
"""

from dataclasses import dataclass, field
from typing import Any


@dataclass
class Event:
    """Plugin-side event mirroring the daemon's broker event shape."""

    topic: str
    source: str
    payload: dict[str, Any]
    correlation_id: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def new(cls, topic: str, source: str, payload: dict[str, Any]) -> "Event":
        return cls(topic=topic, source=source, payload=payload)

    def to_json(self) -> dict[str, Any]:
        out: dict[str, Any] = {
            "topic": self.topic,
            "source": self.source,
            "payload": self.payload,
        }
        if self.correlation_id is not None:
            out["correlation_id"] = self.correlation_id
        if self.metadata:
            out["metadata"] = self.metadata
        return out

    @classmethod
    def from_json(cls, data: dict[str, Any]) -> "Event":
        return cls(
            topic=data["topic"],
            source=data["source"],
            payload=data.get("payload", {}),
            correlation_id=data.get("correlation_id"),
            metadata=data.get("metadata", {}),
        )
