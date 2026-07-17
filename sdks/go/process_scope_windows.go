//go:build windows

package ctxagenthistory

import (
	"fmt"
	"os/exec"
	"syscall"
)

const (
	createSuspended       = 0x00000004
	createNewProcessGroup = 0x00000200
	processSetQuota       = 0x0100
	processTerminate      = 0x0001
	processSuspendResume  = 0x0800
)

var (
	kernel32                 = syscall.NewLazyDLL("kernel32.dll")
	ntdll                    = syscall.NewLazyDLL("ntdll.dll")
	createJobObjectW         = kernel32.NewProc("CreateJobObjectW")
	assignProcessToJobObject = kernel32.NewProc("AssignProcessToJobObject")
	terminateJobObject       = kernel32.NewProc("TerminateJobObject")
	openProcess              = kernel32.NewProc("OpenProcess")
	closeHandle              = kernel32.NewProc("CloseHandle")
	ntResumeProcess          = ntdll.NewProc("NtResumeProcess")
)

type ownedProcessScope struct {
	job syscall.Handle
}

func newOwnedProcessScope(cmd *exec.Cmd) (*ownedProcessScope, error) {
	job, _, callErr := createJobObjectW.Call(0, 0)
	if job == 0 {
		return nil, fmt.Errorf("create ctx CLI job object: %w", callErr)
	}
	cmd.SysProcAttr = &syscall.SysProcAttr{CreationFlags: createSuspended | createNewProcessGroup}
	return &ownedProcessScope{job: syscall.Handle(job)}, nil
}

func (scope *ownedProcessScope) AfterStart(cmd *exec.Cmd) error {
	if cmd.Process == nil {
		return fmt.Errorf("ctx CLI process did not start")
	}
	process, _, callErr := openProcess.Call(
		processSetQuota|processTerminate|processSuspendResume,
		0,
		uintptr(uint32(cmd.Process.Pid)),
	)
	if process == 0 {
		return fmt.Errorf("open suspended ctx CLI process: %w", callErr)
	}
	defer closeHandle.Call(process)
	assigned, _, callErr := assignProcessToJobObject.Call(uintptr(scope.job), process)
	if assigned == 0 {
		return fmt.Errorf("assign ctx CLI process to job object: %w", callErr)
	}
	status, _, _ := ntResumeProcess.Call(process)
	if status != 0 {
		return fmt.Errorf("resume ctx CLI process: NTSTATUS 0x%x", status)
	}
	return nil
}

func (scope *ownedProcessScope) Terminate(cmd *exec.Cmd) {
	if scope.job != 0 {
		terminateJobObject.Call(uintptr(scope.job), 1)
	}
	if cmd.Process != nil {
		_ = cmd.Process.Kill()
	}
}

func (scope *ownedProcessScope) Close() {
	if scope.job != 0 {
		closeHandle.Call(uintptr(scope.job))
		scope.job = 0
	}
}
