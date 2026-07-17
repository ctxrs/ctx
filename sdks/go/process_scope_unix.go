//go:build !windows

package ctxagenthistory

import (
	"os/exec"
	"syscall"
	"time"
)

type ownedProcessScope struct {
	pid        int
	terminated bool
}

func newOwnedProcessScope(cmd *exec.Cmd) (*ownedProcessScope, error) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	return &ownedProcessScope{}, nil
}

func (scope *ownedProcessScope) AfterStart(cmd *exec.Cmd) error {
	if cmd.Process != nil {
		scope.pid = cmd.Process.Pid
	}
	return nil
}

func (scope *ownedProcessScope) Terminate(cmd *exec.Cmd) {
	if scope.pid == 0 && cmd.Process != nil {
		scope.pid = cmd.Process.Pid
	}
	scope.terminate()
	if cmd.Process != nil {
		_ = cmd.Process.Kill()
	}
}

func (scope *ownedProcessScope) terminate() {
	if scope.terminated || scope.pid == 0 {
		return
	}
	scope.terminated = true
	if !processGroupAlive(scope.pid) {
		return
	}
	_ = syscall.Kill(-scope.pid, syscall.SIGTERM)
	deadline := time.Now().Add(localTeardownDelay)
	for processGroupAlive(scope.pid) && time.Now().Before(deadline) {
		time.Sleep(10 * time.Millisecond)
	}
	if processGroupAlive(scope.pid) {
		_ = syscall.Kill(-scope.pid, syscall.SIGKILL)
	}
}

func (scope *ownedProcessScope) Close() {
	scope.terminate()
}

func processGroupAlive(pid int) bool {
	err := syscall.Kill(-pid, 0)
	return err == nil || err == syscall.EPERM
}
