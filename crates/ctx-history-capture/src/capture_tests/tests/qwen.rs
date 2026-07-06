#[allow(unused_imports)]
use super::*;

pub(crate) fn write_qwen_smoke_fixture(temp: &TempDir) -> PathBuf {
    let chats = temp.path().join("qwen/.qwen/projects/workspace/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
        chats.join("qwen-smoke.jsonl"),
        concat!(
            "{\"uuid\":\"qwen-1\",\"parentUuid\":null,\"sessionId\":\"qwen-smoke\",\"timestamp\":\"2026-07-04T12:00:00Z\",\"type\":\"user\",\"cwd\":\"/workspace/qwen\",\"version\":\"test\",\"gitBranch\":\"main\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"qwen jsonl oracle prompt\"}]},\"model\":\"qwen3-coder\"}\n",
            "{\"uuid\":\"qwen-2\",\"parentUuid\":\"qwen-1\",\"sessionId\":\"qwen-smoke\",\"timestamp\":\"2026-07-04T12:00:01Z\",\"type\":\"assistant\",\"cwd\":\"/workspace/qwen\",\"version\":\"test\",\"gitBranch\":\"main\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"qwen jsonl oracle answer\"},{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Write\",\"input\":{\"path\":\"src/qwen_oracle.txt\",\"content\":\"proof\"}}]},\"usageMetadata\":{\"inputTokens\":5,\"outputTokens\":7},\"model\":\"qwen3-coder\"}\n",
            "{\"uuid\":\"qwen-3\",\"parentUuid\":\"qwen-2\",\"sessionId\":\"qwen-smoke\",\"timestamp\":\"2026-07-04T12:00:02Z\",\"type\":\"tool_result\",\"cwd\":\"/workspace/qwen\",\"version\":\"test\",\"gitBranch\":\"main\",\"message\":{\"role\":\"tool\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"wrote src/qwen_oracle.txt\"}]},\"toolCallResult\":{\"tool\":\"Write\",\"path\":\"src/qwen_oracle.txt\",\"output\":\"ok\"},\"model\":\"qwen3-coder\"}\n",
        ),
    )
    .unwrap();
    temp.path().join("qwen/.qwen/projects")
}
