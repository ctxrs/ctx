"""Closed reviewed MCP attribution route-schema contracts."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


ROUTE_CONTRACT_CLASSIFICATION = "route_schema_contract"


@dataclass(frozen=True)
class RouteSchemaContract:
    provider: str
    route: str
    source_format: str
    format_schema: dict[str, Any]
    producer_domain: dict[str, Any]
    path: str
    allowed_subtree: str
    classification: str
    sha256: str

    @property
    def key(self) -> tuple[str, str, str]:
        return (self.provider, self.route, self.source_format)


def _structural(revision: int) -> dict[str, Any]:
    return {"kind": "structural_revision", "revision": revision}


def _embedded_integer(version: int) -> dict[str, Any]:
    return {"kind": "embedded_integer", "version": version}


def _embedded_semver(version: str) -> dict[str, Any]:
    return {"kind": "embedded_semver", "version": version}


def _unversioned(generation: int) -> dict[str, Any]:
    return {"kind": "unversioned_generation", "generation": generation}


def _semver(version: str) -> dict[str, Any]:
    return {"kind": "semver", "version": version}


def _integer(version: int) -> dict[str, Any]:
    return {"kind": "integer", "version": version}


def _calendar_date(version: str) -> dict[str, Any]:
    return {"kind": "calendar_date", "version": version}


def _discrete(*versions: dict[str, Any]) -> dict[str, Any]:
    return {"kind": "discrete", "versions": list(versions)}


ROUTE_SCHEMA_CONTRACTS = (
    RouteSchemaContract(
        provider="codex",
        route="native_import",
        source_format="codex_session_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_semver("0.200.0"), _semver("0.201.0"), _semver("0.202.0"), _unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/codex/codex_session_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/codex/codex_session_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="29a770236fb8476cb870afd74f9ec8a9046c18d34cb6329e3615f0898b129538",
    ),
    RouteSchemaContract(
        provider="codex",
        route="native_import",
        source_format="codex_history_jsonl",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/codex/codex_history_jsonl/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/codex/codex_history_jsonl",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="26d3031938e30649641397cdd37d4384f3a704e7f042e61c62005af5c02dd683",
    ),
    RouteSchemaContract(
        provider="pi",
        route="native_import",
        source_format="pi_session_jsonl",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/pi/pi_session_jsonl/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/pi/pi_session_jsonl",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="312ae120e3bfa7f48dee1fff8900f72f080754ef70c4b6a4c94bf66dc6671e93",
    ),
    RouteSchemaContract(
        provider="claude_code",
        route="native_import",
        source_format="claude_projects_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/claude_code/claude_projects_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/claude_code/claude_projects_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="8528b7ab5f04d133b84c3aeb84398ad7494e66c8dd9dd15d9fba5789d8d909bb",
    ),
    RouteSchemaContract(
        provider="open_code",
        route="native_import",
        source_format="opencode_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/open_code/opencode_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/open_code/opencode_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="ab32c60967eff0ae4b283782d9d1375478b1e7057d645d5a9a001e1f21544ad8",
    ),
    RouteSchemaContract(
        provider="kilo",
        route="native_import",
        source_format="kilo_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/kilo/kilo_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/kilo/kilo_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="8f402029d07f7db7db2bf349ade36026764849a605610923213b0afac2bc4268",
    ),
    RouteSchemaContract(
        provider="mimocode",
        route="native_import",
        source_format="mimocode_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/mimocode/mimocode_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/mimocode/mimocode_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="608630749cd9c88c383b62c6493453774f3a97ffef78980d15809a1ec146b287",
    ),
    RouteSchemaContract(
        provider="kiro_cli",
        route="native_import",
        source_format="kiro_cli_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/kiro_cli/kiro_cli_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/kiro_cli/kiro_cli_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="8c12a2beecb245b10bb0d5c66c5a58d61ffd1805c3114c50929a911ac48c75ec",
    ),
    RouteSchemaContract(
        provider="crush",
        route="native_import",
        source_format="crush_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/crush/crush_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/crush/crush_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="a84fb4d0de1f712eb9f47928d22a0834bfef36df34ec9f7a07db4702f0e3c7e7",
    ),
    RouteSchemaContract(
        provider="goose",
        route="native_import",
        source_format="goose_sessions_sqlite",
        format_schema=_embedded_integer(14),
        producer_domain=_discrete(_integer(14)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/goose/goose_sessions_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/goose/goose_sessions_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="28f5c2799267dd380088934ba9f6fe1ee91ba7faf187b08fe282830f07502204",
    ),
    RouteSchemaContract(
        provider="lingma",
        route="native_import",
        source_format="lingma_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/lingma/lingma_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/lingma/lingma_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="01009dcb3b38a5418c789481339d0d344c4f4f501bf59de9c85394098af2ac06",
    ),
    RouteSchemaContract(
        provider="qoder",
        route="native_import",
        source_format="qoder_transcript_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/qoder/qoder_transcript_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/qoder/qoder_transcript_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="d09d759125e75035c989a2dc50d80fe1c94d33d27d687e4044484780ed57cfe3",
    ),
    RouteSchemaContract(
        provider="warp",
        route="native_import",
        source_format="warp_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/warp/warp_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/warp/warp_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="80a2ea15b4ef1f8a81bda4ba29ddf97ce777f868f49e20aa4a04e28d29d20c3b",
    ),
    RouteSchemaContract(
        provider="codebuddy",
        route="native_import",
        source_format="codebuddy_history_json",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/codebuddy/codebuddy_history_json/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/codebuddy/codebuddy_history_json",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="59811cb3ff2bfbbd2539ae4f3bde1bbade05aff7c0ddacbda0f806cc32481a38",
    ),
    RouteSchemaContract(
        provider="trae",
        route="native_import",
        source_format="trae_state_vscdb",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/trae/trae_state_vscdb/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/trae/trae_state_vscdb",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="611d3fa61c4c7dffddaedf583f533aa23541a5f6580acb5eb0e6fbf1c491a92c",
    ),
    RouteSchemaContract(
        provider="openclaw",
        route="native_import",
        source_format="openclaw_session_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/openclaw/openclaw_session_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/openclaw/openclaw_session_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="50bf882171d86e819e9cecb7102e13c1a986fb7b116c7862e59669362a5989d5",
    ),
    RouteSchemaContract(
        provider="hermes",
        route="native_import",
        source_format="hermes_state_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/hermes/hermes_state_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/hermes/hermes_state_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="a8b8c0ac1c965c69ad04af96d617c1db3d3c3754717aa37cdb091d148188a9a6",
    ),
    RouteSchemaContract(
        provider="nanoclaw",
        route="native_import",
        source_format="nanoclaw_project",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/nanoclaw/nanoclaw_project/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/nanoclaw/nanoclaw_project",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="d59453b912a787e434bbcd533ad8e8ae1c3ae2c355f2842dc5f62d9239c5656c",
    ),
    RouteSchemaContract(
        provider="astrbot",
        route="native_import",
        source_format="astrbot_data_v4_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/astrbot/astrbot_data_v4_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/astrbot/astrbot_data_v4_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="c15b3c07062e80a48ef15a4eb7edf3c936e026435f9ac9da42b69234d3fd4fbf",
    ),
    RouteSchemaContract(
        provider="shelley",
        route="native_import",
        source_format="shelley_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/shelley/shelley_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/shelley/shelley_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="47196da8b691e3beb25f326bd291b1bc65eb4cb395042e05df9502e02d603bd6",
    ),
    RouteSchemaContract(
        provider="continue",
        route="native_import",
        source_format="continue_cli_sessions_json",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/continue/continue_cli_sessions_json/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/continue/continue_cli_sessions_json",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="c2afc94dc5dfd7635603302dff1299d23c22f63e4910092a36c9a1dc3d00d022",
    ),
    RouteSchemaContract(
        provider="openhands",
        route="native_import",
        source_format="openhands_file_events",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/openhands/openhands_file_events/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/openhands/openhands_file_events",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="31b6d6cc22cd29da84bc5fd2bdc8e78737e6537a7bec8be59b36e9a4a4c43059",
    ),
    RouteSchemaContract(
        provider="antigravity_cli",
        route="native_import",
        source_format="antigravity_cli_transcript_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/antigravity_cli/antigravity_cli_transcript_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/antigravity_cli/antigravity_cli_transcript_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="8bc46c32ad1b44b7b739d9710b934bf257104780e524676f9b2691b023cbb138",
    ),
    RouteSchemaContract(
        provider="gemini_cli",
        route="native_import",
        source_format="gemini_cli_chat_recording_jsonl",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/gemini_cli/gemini_cli_chat_recording_jsonl/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/gemini_cli/gemini_cli_chat_recording_jsonl",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="c595460f4c7be4c6a733fb82f8e89651edcc8fb144ff9c91874a9ecd6ecfdabb",
    ),
    RouteSchemaContract(
        provider="tabnine",
        route="native_import",
        source_format="tabnine_cli_chat_recording_jsonl",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/tabnine/tabnine_cli_chat_recording_jsonl/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/tabnine/tabnine_cli_chat_recording_jsonl",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="44e8b4745339c8c8f6ded23c213c39ac46a519e7a4eca2ce18f4a2203b8bf92d",
    ),
    RouteSchemaContract(
        provider="cursor",
        route="native_import",
        source_format="cursor_agent_transcript_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_calendar_date("2026-06-24")),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/cursor/cursor_agent_transcript_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/cursor/cursor_agent_transcript_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="d3acdb294a7dbad229abca076f4270b48be2a225feb8f2a0ad79ead1e0f45326",
    ),
    RouteSchemaContract(
        provider="windsurf",
        route="native_import",
        source_format="windsurf_cascade_hook_transcript_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/windsurf/windsurf_cascade_hook_transcript_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/windsurf/windsurf_cascade_hook_transcript_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="44be3095787812a224ad320fe1dda479e7f0ae9669403fbf8edf40a5a0b5b15a",
    ),
    RouteSchemaContract(
        provider="zed",
        route="native_import",
        source_format="zed_threads_sqlite",
        format_schema=_embedded_semver("0.3.0"),
        producer_domain=_discrete(_semver("0.3.0")),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/zed/zed_threads_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/zed/zed_threads_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="111306f4241f51c1ae407c28fff194c4dd90e5e54237bb0910a91e7241dab1ee",
    ),
    RouteSchemaContract(
        provider="copilot_cli",
        route="native_import",
        source_format="copilot_cli_session_events_jsonl",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/copilot_cli/copilot_cli_session_events_jsonl/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/copilot_cli/copilot_cli_session_events_jsonl",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="06fcbf74ad103a010480c7698063ef2430dfc1ffb07345b54ff63d2014591315",
    ),
    RouteSchemaContract(
        provider="factory_ai_droid",
        route="native_import",
        source_format="factory_ai_droid_sessions_jsonl",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/factory_ai_droid/factory_ai_droid_sessions_jsonl/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/factory_ai_droid/factory_ai_droid_sessions_jsonl",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="7659b24671c901e785c14195b9ef73cd11c9de5d0369410523f33e4b33d2fb69",
    ),
    RouteSchemaContract(
        provider="qwen_code",
        route="native_import",
        source_format="qwen_code_chat_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/qwen_code/qwen_code_chat_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/qwen_code/qwen_code_chat_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="dfc2c21cdcb12243f7c92fd44011e9cd38eddda79c5ae4a82d64a794f2138351",
    ),
    RouteSchemaContract(
        provider="kimi_code_cli",
        route="native_import",
        source_format="kimi_code_cli_wire_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/kimi_code_cli/kimi_code_cli_wire_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/kimi_code_cli/kimi_code_cli_wire_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="81f726f19bd5b594913e674c2026d13f8249f1c76c6cd2137a1173d8f8d8756b",
    ),
    RouteSchemaContract(
        provider="auggie",
        route="native_import",
        source_format="auggie_session_json",
        format_schema=_structural(1),
        producer_domain=_discrete(_semver("0.32.0")),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/auggie/auggie_session_json/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/auggie/auggie_session_json",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="799b7f14644b67be7d63815debfee8214fa4dc8b75e46c2232a39137da8fc90f",
    ),
    RouteSchemaContract(
        provider="junie",
        route="native_import",
        source_format="junie_session_events_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/junie/junie_session_events_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/junie/junie_session_events_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="7773c6294853954138c0c092433807eda800f3802af67faf4c40d2bbbf6ae934",
    ),
    RouteSchemaContract(
        provider="firebender",
        route="native_import",
        source_format="firebender_chat_history_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/firebender/firebender_chat_history_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/firebender/firebender_chat_history_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="5103eb68d01a2d87a64234a7ba8450338fa926390eef4ca0238fa6deed42d5cd",
    ),
    RouteSchemaContract(
        provider="forgecode",
        route="native_import",
        source_format="forgecode_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/forgecode/forgecode_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/forgecode/forgecode_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="c0349f257a7d2018dcf44d31508691ad36d69f40c2056f6c092b74b1aa3b735d",
    ),
    RouteSchemaContract(
        provider="deepagents",
        route="native_import",
        source_format="deepagents_sessions_sqlite",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/deepagents/deepagents_sessions_sqlite/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/deepagents/deepagents_sessions_sqlite",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="45909d6839401de18bc1f786a141dd23593767e884352fb9373fe6181a95a3f3",
    ),
    RouteSchemaContract(
        provider="mistral_vibe",
        route="native_import",
        source_format="mistral_vibe_session_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/mistral_vibe/mistral_vibe_session_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/mistral_vibe/mistral_vibe_session_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="78e8623d873853238b9d3f708b6ed3936d30b12fe0a1ae6f913a4f376ba39f43",
    ),
    RouteSchemaContract(
        provider="mux",
        route="native_import",
        source_format="mux_session_jsonl_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_semver("0.27.0")),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/mux/mux_session_jsonl_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/mux/mux_session_jsonl_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="458c2b2840f0a46a36d30bf0fbbbaa58b20cce2b040d428bd9b8e955446eb34a",
    ),
    RouteSchemaContract(
        provider="rovodev",
        route="native_import",
        source_format="rovodev_session_json_tree",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/rovodev/rovodev_session_json_tree/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/rovodev/rovodev_session_json_tree",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="a4f64eaf4e8f51d9b8032a7c1139c87731edc887a883dc826845fa2acd8a3e89",
    ),
    RouteSchemaContract(
        provider="cline",
        route="native_import",
        source_format="cline_task_directory_json",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/cline/cline_task_directory_json/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/cline/cline_task_directory_json",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="22bddf16863c2d29d15a5763e25f1b6168d9aeef21bc2b4abfdfa3e50fa78468",
    ),
    RouteSchemaContract(
        provider="roo_code",
        route="native_import",
        source_format="roo_task_directory_json",
        format_schema=_structural(1),
        producer_domain=_discrete(_unversioned(1)),
        path="crates/ctx-history-capture/tests/contracts/mcp-attribution/roo_code/roo_task_directory_json/shape-contract.json",
        allowed_subtree="crates/ctx-history-capture/tests/contracts/mcp-attribution/roo_code/roo_task_directory_json",
        classification=ROUTE_CONTRACT_CLASSIFICATION,
        sha256="473a32367bc738909da0082b1f18dfbb8a06ed44c00951f60f5c677df0ec6ad3",
    ),
)


FIXTURE_ROUTE_SCHEMA_CONTRACT = RouteSchemaContract(
    provider="fixture",
    route="native_import",
    source_format="fixture_jsonl",
    format_schema=_structural(1),
    producer_domain=_discrete(_unversioned(1)),
    path=(
        "crates/ctx-history-capture/tests/contracts/mcp-attribution-fixtures/"
        "fixture/fixture_jsonl/shape-contract.json"
    ),
    allowed_subtree=(
        "crates/ctx-history-capture/tests/contracts/mcp-attribution-fixtures/"
        "fixture/fixture_jsonl"
    ),
    classification=ROUTE_CONTRACT_CLASSIFICATION,
    sha256="817f673c3f860988efb64c55cfd682dbd7e41db13d311dafb5715f0ea31125f7",
)
