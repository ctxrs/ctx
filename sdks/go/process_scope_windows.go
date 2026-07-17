//go:build windows

package ctxagenthistory

import (
	"os/exec"
	"strconv"
	"syscall"
	"time"
)

func configureProcessScope(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{CreationFlags: syscall.CREATE_NEW_PROCESS_GROUP}
}

func terminateProcessScope(cmd *exec.Cmd) {
	if cmd.Process == nil {
		return
	}
	killer := exec.Command("taskkill", "/PID", strconv.Itoa(cmd.Process.Pid), "/T", "/F")
	if err := killer.Start(); err == nil {
		done := make(chan struct{})
		go func() {
			_ = killer.Wait()
			close(done)
		}()
		select {
		case <-done:
		case <-time.After(5 * localTeardownDelay):
			_ = killer.Process.Kill()
		}
	}
	_ = cmd.Process.Kill()
}
