"""
WuppieFuzz Crash Deduplication Benchmark Application
=====================================================
A synthetic FastAPI app designed to stress-test crash deduplication algorithms.
Each intentional crash carries an X-Benchmark-Crash-ID header with the true bug ID.
Normal (non-crashing) responses never carry that header.

Benchmark case classes implemented:
  A. Simple distinct bugs           → BUG-001, BUG-002
  B. Duplicate-heavy same root      → BUG-003
  C. False-merge challenge          → BUG-004, BUG-005
  D. False-split by endpoint        → BUG-006
  E. False-split by response class  → BUG-007
  F. False-split by status code     → BUG-008
  G. Validation-style (non-5xx)     → BUG-009
  H. Stateful / sequence bug        → BUG-010
  I. Deterministic path input       → BUG-011
  J. Body-shape bug                 → BUG-012
  K. Same-endpoint distinct bugs    → BUG-013, BUG-014
  L. Malformed / invalid JSON body  → BUG-015
"""

from __future__ import annotations

from typing import Any, Optional

import json
from fastapi import FastAPI, Request, Path, Query
from fastapi.responses import JSONResponse, PlainTextResponse, HTMLResponse, Response
from pydantic import BaseModel

# ---------------------------------------------------------------------------
# Central exception type
# ---------------------------------------------------------------------------

class BenchmarkCrash(Exception):
    """
    Raised whenever an endpoint hits an intentional benchmark bug.

    Fields
    ------
    bug_id         : The oracle ID, e.g. "BUG-001".
    status_code    : HTTP status code to return.
    response_class : One of "json" | "text" | "html" | "empty" | "invalid_json".
    detail         : Human-readable description embedded in the response body.
    """

    def __init__(
            self,
            bug_id: str,
            status_code: int,
            response_class: str,
            detail: str,
    ) -> None:
        super().__init__(detail)
        self.bug_id = bug_id
        self.status_code = status_code
        self.response_class = response_class
        self.detail = detail


# ---------------------------------------------------------------------------
# Application and exception handler
# ---------------------------------------------------------------------------

app = FastAPI(
    title="WuppieFuzz Deduplication Benchmark",
    version="1.0.0",
    description=(
        "Synthetic benchmark app for evaluating crash deduplication quality. "
        "Intentional crashes carry X-Benchmark-Crash-ID; normal responses do not."
    ),
)


@app.exception_handler(BenchmarkCrash)
async def benchmark_crash_handler(request: Request, exc: BenchmarkCrash) -> Response:
    """
    Convert a BenchmarkCrash into an HTTP response with the oracle header.
    The response body format is controlled by exc.response_class.
    """
    oracle_header = {"X-Benchmark-Crash-ID": exc.bug_id}

    if exc.response_class == "json":
        return JSONResponse(
            status_code=exc.status_code,
            content={"error": exc.detail, "bug_id": exc.bug_id},
            headers=oracle_header,
        )

    if exc.response_class == "text":
        return PlainTextResponse(
            status_code=exc.status_code,
            content=f"ERROR [{exc.bug_id}]: {exc.detail}",
            headers=oracle_header,
        )

    if exc.response_class == "html":
        body = (
            f"<html><body>"
            f"<h1>Error {exc.status_code}</h1>"
            f"<p>{exc.detail}</p>"
            f"<code>{exc.bug_id}</code>"
            f"</body></html>"
        )
        return HTMLResponse(
            status_code=exc.status_code,
            content=body,
            headers=oracle_header,
        )

    if exc.response_class == "empty":
        return Response(
            status_code=exc.status_code,
            headers=oracle_header,
        )

    if exc.response_class == "invalid_json":
        # Deliberately truncated / malformed JSON — looks like JSON but is not valid.
        return Response(
            status_code=exc.status_code,
            content='{"error":',
            media_type="application/json",
            headers=oracle_header,
        )

    # Fallback — should never be reached with well-formed code.
    return JSONResponse(
        status_code=exc.status_code,
        content={"error": exc.detail},
        headers=oracle_header,
    )


# ---------------------------------------------------------------------------
# Mutable benchmark state (fully resettable)
# ---------------------------------------------------------------------------

class BenchmarkState:
    """All mutable state lives here so POST /__benchmark/reset can wipe it cleanly."""

    def __init__(self) -> None:
        self.tokens: set[str] = set()

    def reset(self) -> None:
        self.tokens.clear()


STATE = BenchmarkState()


# ---------------------------------------------------------------------------
# A. Simple distinct bugs
# ---------------------------------------------------------------------------

@app.get(
    "/math/divide",
    summary="Divide two integers",
    tags=["A – Simple distinct bugs"],
)
def math_divide(
        numerator: int = Query(..., description="Dividend"),
        denominator: int = Query(..., description="Divisor"),
) -> dict[str, Any]:
    """
    BUG-001 — divide-by-zero style crash.
    Triggered when denominator == 0.
    Simple, isolated bug; expected to form a clean singleton cluster.
    """
    if denominator == 0:
        raise BenchmarkCrash(
            bug_id="BUG-001",
            status_code=500,
            response_class="json",
            detail="Division by zero: denominator must not be zero.",
        )
    return {"result": numerator / denominator}


@app.get(
    "/users/{user_id}",
    summary="Fetch a user by ID",
    tags=["A – Simple distinct bugs"],
)
def get_user(
        user_id: int = Path(..., description="User ID (must be positive)"),
) -> dict[str, Any]:
    """
    BUG-002 — negative user ID crash.
    Triggered when user_id < 0.
    Different endpoint, different trigger condition from BUG-001.
    """
    if user_id < 0:
        raise BenchmarkCrash(
            bug_id="BUG-002",
            status_code=500,
            response_class="json",
            detail=f"Invalid user ID {user_id}: negative IDs are not allowed.",
        )
    return {"user_id": user_id, "name": f"User_{user_id}", "active": True}


# ---------------------------------------------------------------------------
# B. Duplicate-heavy same-root-cause bug
# ---------------------------------------------------------------------------

_CRASH_QUERIES: frozenset[str] = frozenset(
    {"crash", "panic", "boom", "explode", "segfault", "abort", "fatal"}
)


@app.get(
    "/search",
    summary="Search for items",
    tags=["B – Duplicate-heavy same root cause"],
)
def search(
        q: str = Query(..., description="Search query string"),
) -> dict[str, Any]:
    """
    BUG-003 — search engine crash on several distinct trigger words.
    Multiple different query strings all map to the same underlying bug.
    A good deduplicator should merge all of these into one BUG-003 cluster.
    A poor deduplicator might create one cluster per unique query string.
    """
    if q.lower() in _CRASH_QUERIES:
        raise BenchmarkCrash(
            bug_id="BUG-003",
            status_code=500,
            response_class="json",
            detail=f"Search engine panic triggered by query: {q!r}",
        )
    return {"query": q, "results": [], "total": 0}


# ---------------------------------------------------------------------------
# C. False-merge challenge — two bugs, same endpoint/status/response class
# ---------------------------------------------------------------------------

class ParseRequest(BaseModel):
    content: str
    format: str = "json"  # "json" | "xml"


@app.post(
    "/parser",
    summary="Parse structured content",
    tags=["C – False-merge challenge"],
)
def parse_content(body: ParseRequest) -> dict[str, Any]:
    """
    BUG-004 — JSON parser internal crash (triggered when format=='json' and
               content contains '{{crash}}').
    BUG-005 — XML parser internal crash  (triggered when format=='xml' and
               content contains '<crash/>').

    Both return HTTP 500 JSON from the same endpoint.
    A coarse deduplicator sees: (exit=crash, kind=500, class=json, endpoint=POST /parser)
    and will MERGE them into one cluster — a false merge.
    """
    if body.format == "json" and "{{crash}}" in body.content:
        raise BenchmarkCrash(
            bug_id="BUG-004",
            status_code=500,
            response_class="json",
            detail="JSON parser internal error: unexpected token in lookahead.",
        )
    if body.format == "xml" and "<crash/>" in body.content:
        raise BenchmarkCrash(
            bug_id="BUG-005",
            status_code=500,
            response_class="json",
            detail="XML parser internal error: malformed namespace declaration.",
        )
    return {"parsed": True, "format": body.format, "length": len(body.content)}


# ---------------------------------------------------------------------------
# D. False-split challenge by endpoint — same bug, different routes
# ---------------------------------------------------------------------------

def _trigger_cache_corruption(source_endpoint: str) -> None:
    """
    Shared internal helper that raises BUG-006 from two distinct endpoints.
    Because endpoint is part of the dedup identity, a coarse deduplicator
    will SPLIT BUG-006 into two clusters: one for /cache/read, one for /cache/export.
    """
    raise BenchmarkCrash(
        bug_id="BUG-006",
        status_code=500,
        response_class="json",
        detail=f"Cache corruption detected in {source_endpoint}: checksum mismatch.",
    )


@app.get(
    "/cache/read",
    summary="Read from the cache",
    tags=["D – False-split by endpoint"],
)
def cache_read(corrupt: bool = Query(False, description="Simulate cache corruption")) -> dict[str, Any]:
    """
    BUG-006 (reachable here) — shared cache corruption bug.
    Trigger: ?corrupt=true
    Same true bug as reachable from /cache/export.
    """
    if corrupt:
        _trigger_cache_corruption("/cache/read")
    return {"cache": "hit", "value": "data"}


@app.get(
    "/cache/export",
    summary="Export the cache to a snapshot",
    tags=["D – False-split by endpoint"],
)
def cache_export(corrupt: bool = Query(False, description="Simulate cache corruption")) -> dict[str, Any]:
    """
    BUG-006 (reachable here) — same shared cache corruption bug.
    Trigger: ?corrupt=true
    A fuzzer that finds this path gets a different endpoint in the dedup key,
    causing the deduplicator to create a second cluster for what is really BUG-006.
    """
    if corrupt:
        _trigger_cache_corruption("/cache/export")
    return {"snapshot": "ok", "records": 0}


# ---------------------------------------------------------------------------
# E. False-split challenge by response class — same bug, varying body format
# ---------------------------------------------------------------------------

@app.get(
    "/render",
    summary="Render a template",
    tags=["E – False-split by response class"],
)
def render_template(
        template: str = Query(..., description="Template name to render"),
        format: str = Query("json", description="Response format: json | text | html"),
) -> Response:
    """
    BUG-007 — template rendering crash on template=='crash'.
    The response class (json/text/html) is determined by the ?format= parameter.
    A coarse deduplicator splits this into up to three clusters — one per response
    class — even though a single bug causes all of them.
    """
    if template == "crash":
        if format == "text":
            rc = "text"
        elif format == "html":
            rc = "html"
        else:
            rc = "json"
        raise BenchmarkCrash(
            bug_id="BUG-007",
            status_code=500,
            response_class=rc,
            detail="Template engine segfault: null pointer in render pipeline.",
        )
    return JSONResponse(content={"rendered": f"<p>Hello from {template}</p>", "format": format})


# ---------------------------------------------------------------------------
# F. False-split challenge by status code — same bug, different HTTP codes
# ---------------------------------------------------------------------------

@app.get(
    "/legacy",
    summary="Legacy compatibility endpoint",
    tags=["F – False-split by status code"],
)
def legacy(
        mode: str = Query("v1", description="Compatibility mode: v1 (→500) or v2 (→502)"),
) -> dict[str, Any]:
    """
    BUG-008 — legacy compatibility layer crash.
    mode='v1' → HTTP 500, mode='v2' → HTTP 502.
    Same root cause, but different status codes in the response.
    A deduplicator that keys on status code will SPLIT this into two clusters.
    """
    if mode in ("v1", "v2"):
        status = 500 if mode == "v1" else 502
        raise BenchmarkCrash(
            bug_id="BUG-008",
            status_code=status,
            response_class="json",
            detail=f"Legacy bridge crash in mode={mode!r}: unsupported protocol version.",
        )
    return {"mode": mode, "status": "ok"}


# ---------------------------------------------------------------------------
# G. Validation-style bug — non-5xx, but unexpected per spec
# ---------------------------------------------------------------------------

@app.get(
    "/strict/{item_id}",
    summary="Strict item lookup",
    tags=["G – Validation-style bug"],
)
def strict_item(
        item_id: int = Path(..., description="Item ID (0 triggers BUG-009)"),
) -> dict[str, Any]:
    """
    BUG-009 — returns HTTP 400 with oracle header for item_id == 0.
    The OpenAPI spec documents a normal success response; a fuzzer configured to
    treat unexpected 400s as validation-style crashes should flag this.
    This tests whether the deduplicator handles non-5xx crashes correctly.
    """
    if item_id == 0:
        raise BenchmarkCrash(
            bug_id="BUG-009",
            status_code=400,
            response_class="json",
            detail="Strict validation error: item_id zero is a reserved sentinel and must never reach this handler.",
        )
    return {"item_id": item_id, "value": f"item-{item_id}"}


# ---------------------------------------------------------------------------
# H. Stateful / sequence-dependent bug
# ---------------------------------------------------------------------------

class TokenCreate(BaseModel):
    token: str


class TokenCreated(BaseModel):
    token: str
    created: bool


@app.post(
    "/tokens",
    summary="Create / register a token",
    tags=["H – Stateful sequence bug"],
    status_code=201,
    response_model=TokenCreated,
)
def create_token(body: TokenCreate) -> TokenCreated:
    """
    Step 1 of the stateful sequence: register a token.
    The magic token 'token-admin-crash' must be created before BUG-010 can fire.
    """
    STATE.tokens.add(body.token)
    return TokenCreated(token=body.token, created=True)


@app.get(
    "/tokens/{token}/verify",
    summary="Verify a previously created token",
    tags=["H – Stateful sequence bug"],
)
def verify_token(token: str = Path(..., description="Token string to verify")) -> dict[str, Any]:
    """
    BUG-010 — admin-token privilege escalation crash.
    Only fires when 'token-admin-crash' was previously registered via POST /tokens.
    This tests whether the deduplicator (and the fuzzer) can handle bugs that
    require a specific multi-step setup sequence.
    POST /__benchmark/reset clears token state so the sequence can be replayed cleanly.
    """
    if token == "token-admin-crash" and token in STATE.tokens:
        raise BenchmarkCrash(
            bug_id="BUG-010",
            status_code=500,
            response_class="json",
            detail="Privilege escalation panic: admin token reached verification handler without ACL check.",
        )
    if token not in STATE.tokens:
        return JSONResponse(status_code=404, content={"error": "token not found"})
    return {"token": token, "valid": True}


# ---------------------------------------------------------------------------
# I. Deterministic path / input bug
# ---------------------------------------------------------------------------

@app.get(
    "/flaky/{resource_id}",
    summary="Fetch a resource (deterministically fails for resource_id % 7 == 0)",
    tags=["I – Deterministic path bug"],
)
def flaky_resource(
        resource_id: int = Path(..., description="Resource ID"),
) -> dict[str, Any]:
    """
    BUG-011 — crashes when resource_id % 7 == 0 (i.e. 0, 7, 14, 21, …).
    Appears 'flaky' to a fuzzer that doesn't understand the pattern, but is
    fully deterministic — useful for measuring how replay-based deduplication
    handles inputs that seem intermittent but are actually stable.
    """
    if resource_id % 7 == 0:
        raise BenchmarkCrash(
            bug_id="BUG-011",
            status_code=500,
            response_class="json",
            detail=f"Resource {resource_id} hit a deterministic crash boundary (id % 7 == 0).",
        )
    return {"resource_id": resource_id, "data": f"payload-{resource_id}"}


# ---------------------------------------------------------------------------
# J. Body-shape bug
# ---------------------------------------------------------------------------

class TransformRequest(BaseModel):
    mode: str
    payload: Optional[dict[str, Any]] = None


@app.post(
    "/transform",
    summary="Transform a payload",
    tags=["J – Body-shape bug"],
)
def transform(body: TransformRequest) -> dict[str, Any]:
    """
    BUG-012 — crashes on a very specific request body shape:
      {"mode": "legacy", "payload": {"type": "bomb"}}
    Tests whether the deduplicator can handle crashes triggered by deep body
    structure rather than simple query parameter values.
    """
    if (
            body.mode == "legacy"
            and isinstance(body.payload, dict)
            and body.payload.get("type") == "bomb"
    ):
        raise BenchmarkCrash(
            bug_id="BUG-012",
            status_code=500,
            response_class="json",
            detail="Transform pipeline panic: legacy mode received a disallowed payload shape.",
        )
    return {"mode": body.mode, "transformed": True}


# ---------------------------------------------------------------------------
# K. Two distinct bugs on the same shared endpoint (BUG-013 / BUG-014)
# ---------------------------------------------------------------------------

class ReportRequest(BaseModel):
    report_type: str
    include_pii: bool = False


@app.post(
    "/reports/generate",
    summary="Generate a report",
    tags=["K – Same-endpoint distinct bugs"],
)
def generate_report(body: ReportRequest) -> dict[str, Any]:
    """
    BUG-013 — report type 'financial_audit' triggers an integer overflow in the
               accounting sub-module.
    BUG-014 — include_pii=True with report_type='gdpr_export' triggers a
               null-pointer in the anonymisation layer.

    Both return HTTP 500 JSON from the same endpoint.
    A coarse deduplicator (keying on endpoint + status + response class) will
    MERGE BUG-013 and BUG-014 — a false merge similar to the C case but on a
    different endpoint, giving the benchmark more statistical signal.
    """
    if body.report_type == "financial_audit":
        raise BenchmarkCrash(
            bug_id="BUG-013",
            status_code=500,
            response_class="json",
            detail="Report generator overflow: financial_audit exceeded int32 accumulator.",
        )
    if body.report_type == "gdpr_export" and body.include_pii:
        raise BenchmarkCrash(
            bug_id="BUG-014",
            status_code=500,
            response_class="json",
            detail="Anonymisation layer null-pointer: PII scrubber received uninitialised field.",
        )
    return {"report_type": body.report_type, "status": "queued", "job_id": 42}


# ---------------------------------------------------------------------------
# L. Malformed / invalid JSON response bug (BUG-015)
# ---------------------------------------------------------------------------

@app.get(
    "/export/raw",
    summary="Export raw data (returns malformed JSON on error)",
    tags=["L – Invalid JSON response bug"],
)
def export_raw(
        poison: bool = Query(False, description="Trigger BUG-015 malformed response"),
) -> dict[str, Any]:
    """
    BUG-015 — when ?poison=true, the export pipeline crashes mid-serialisation
    and returns a truncated / invalid JSON body with Content-Type: application/json.
    This tests whether the deduplicator correctly classifies the response class
    as 'invalid_json' rather than 'json', and whether it handles that class at all.
    """
    if poison:
        raise BenchmarkCrash(
            bug_id="BUG-015",
            status_code=500,
            response_class="invalid_json",
            detail="Export serialiser panic: stream truncated mid-write.",
        )
    return {"records": [], "total": 0}


# ---------------------------------------------------------------------------
# Benchmark control endpoint
# ---------------------------------------------------------------------------

@app.post(
    "/__benchmark/reset",
    summary="Reset all mutable benchmark state",
    tags=["Benchmark control"],
    status_code=200,
)
def benchmark_reset() -> dict[str, Any]:
    """
    Clears all internal state (token store, etc.) so stateful tests can be
    replayed from a clean slate.  Must be called between independent test runs
    that involve stateful endpoints (H-class bugs).
    """
    STATE.reset()
    return {"reset": True, "state": "clean"}


# ---------------------------------------------------------------------------
# Health check (no oracle header ever)
# ---------------------------------------------------------------------------

@app.get("/__benchmark/health", tags=["Benchmark control"])
def health() -> dict[str, Any]:
    """Simple liveness probe — always returns 200 with no oracle header."""
    return {"status": "ok", "tokens_registered": len(STATE.tokens)}
