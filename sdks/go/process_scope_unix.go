//go:build !windows

package ctxagenthistory

import (
	"os/exec"
	"syscall"
	"time"
)

type ownedProcessScope struct{}

func newOwnedProcessScope(cmd *exec.Cmd) (*ownedProcessScope, error) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	return &ownedProcessScope{}, nil
}

func (*ownedProcessScope) AfterStart(*exec.Cmd) error {
	return nil
}

func (*ownedProcessScope) Terminate(cmd *exec.Cmd) {
	if cmd.Process == nil {
		return
	}
	pid := cmd.Process.Pid
	_ = syscall.Kill(-pid, syscall.SIGTERM)
	time.Sleep(localTeardownDelay)
	_ = syscall.Kill(-pid, syscall.SIGKILL)
	_ = cmd.Process.Kill()
}

func (*ownedProcessScope) Close() {}
