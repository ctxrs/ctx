package rs.ctx.agenthistory;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Objects;

/** Adapter-neutral agent-history-v2 operation executed by a transport. */
public final class AgentHistoryOperation {
    private final String name;
    private final List<String> args;

    public AgentHistoryOperation(String name, List<String> args) {
        this.name = Objects.requireNonNull(name, "name");
        this.args = Collections.unmodifiableList(new ArrayList<>(Objects.requireNonNull(args, "args")));
    }

    public String name() {
        return name;
    }

    public List<String> args() {
        return args;
    }
}

