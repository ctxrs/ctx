//go:build aix || darwin || dragonfly || freebsd || linux || netbsd || openbsd || solaris

package ctxagenthistory

import (
	"os"
	"os/exec"
	"syscall"
)

type posixProcessTree struct {
	process *os.Process
	group   int
}

func startTaskProcess(cmd *exec.Cmd) (taskProcessTree, error) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		return nil, err
	}
	return &posixProcessTree{process: cmd.Process, group: cmd.Process.Pid}, nil
}

func (p *posixProcessTree) gracefulTerminate() {
	_ = syscall.Kill(-p.group, syscall.SIGTERM)
}

func (p *posixProcessTree) forceTerminate() {
	_ = syscall.Kill(-p.group, syscall.SIGKILL)
	_ = p.process.Kill()
}

func (p *posixProcessTree) close() {
	p.forceTerminate()
}
