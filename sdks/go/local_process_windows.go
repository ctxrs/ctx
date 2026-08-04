//go:build windows

package ctxagenthistory

import (
	"errors"
	"os"
	"os/exec"
	"syscall"
	"unsafe"
)

const (
	createNewProcessGroup           = 0x00000200
	createSuspended                 = 0x00000004
	ctrlBreakEvent                  = 1
	jobObjectExtendedLimitInfoClass = 9
	jobObjectLimitKillOnJobClose    = 0x00002000
	processQueryInformation         = 0x0400
	processSetQuota                 = 0x0100
	processTerminate                = 0x0001
	th32csSnapThread                = 0x00000004
	threadSuspendResume             = 0x0002
)

var (
	kernel32                 = syscall.NewLazyDLL("kernel32.dll")
	assignProcessToJobObject = kernel32.NewProc("AssignProcessToJobObject")
	createJobObjectW         = kernel32.NewProc("CreateJobObjectW")
	createToolhelp32Snapshot = kernel32.NewProc("CreateToolhelp32Snapshot")
	generateConsoleCtrlEvent = kernel32.NewProc("GenerateConsoleCtrlEvent")
	openThread               = kernel32.NewProc("OpenThread")
	resumeThread             = kernel32.NewProc("ResumeThread")
	setInformationJobObject  = kernel32.NewProc("SetInformationJobObject")
	terminateJobObject       = kernel32.NewProc("TerminateJobObject")
	thread32First            = kernel32.NewProc("Thread32First")
	thread32Next             = kernel32.NewProc("Thread32Next")
)

type windowsProcessTree struct {
	job          syscall.Handle
	process      *os.Process
	processGroup uint32
}

func startTaskProcess(cmd *exec.Cmd) (taskProcessTree, error) {
	job, _, callErr := createJobObjectW.Call(0, 0)
	if job == 0 {
		return nil, nonzeroCallError(callErr)
	}
	tree := &windowsProcessTree{job: syscall.Handle(job)}
	var information jobObjectExtendedLimitInformation
	information.basicLimitInformation.limitFlags = jobObjectLimitKillOnJobClose
	set, _, callErr := setInformationJobObject.Call(
		job,
		jobObjectExtendedLimitInfoClass,
		uintptr(unsafe.Pointer(&information)),
		unsafe.Sizeof(information),
	)
	if set == 0 {
		tree.close()
		return nil, nonzeroCallError(callErr)
	}

	cmd.SysProcAttr = &syscall.SysProcAttr{
		CreationFlags: createNewProcessGroup | createSuspended,
	}
	if err := cmd.Start(); err != nil {
		tree.close()
		return nil, err
	}
	tree.process = cmd.Process
	tree.processGroup = uint32(cmd.Process.Pid)
	process, err := syscall.OpenProcess(
		processQueryInformation|processSetQuota|processTerminate,
		false,
		uint32(cmd.Process.Pid),
	)
	if err != nil {
		tree.abortStart(cmd)
		return nil, err
	}
	assigned, _, callErr := assignProcessToJobObject.Call(job, uintptr(process))
	syscall.CloseHandle(process)
	if assigned == 0 {
		tree.abortStart(cmd)
		return nil, nonzeroCallError(callErr)
	}
	if err := resumeProcessThreads(uint32(cmd.Process.Pid)); err != nil {
		tree.abortStart(cmd)
		return nil, err
	}
	return tree, nil
}

func (p *windowsProcessTree) gracefulTerminate() {
	generateConsoleCtrlEvent.Call(ctrlBreakEvent, uintptr(p.processGroup))
}

func (p *windowsProcessTree) forceTerminate() {
	terminateJobObject.Call(uintptr(p.job), 1)
	if p.process != nil {
		_ = p.process.Kill()
	}
}

func (p *windowsProcessTree) close() {
	if p.job != 0 {
		_ = syscall.CloseHandle(p.job)
		p.job = 0
	}
}

func (p *windowsProcessTree) abortStart(cmd *exec.Cmd) {
	p.forceTerminate()
	_ = cmd.Wait()
	p.close()
}

func resumeProcessThreads(processID uint32) error {
	snapshot, _, callErr := createToolhelp32Snapshot.Call(th32csSnapThread, 0)
	if snapshot == ^uintptr(0) {
		return nonzeroCallError(callErr)
	}
	defer syscall.CloseHandle(syscall.Handle(snapshot))
	entry := threadEntry32{size: uint32(unsafe.Sizeof(threadEntry32{}))}
	found, _, callErr := thread32First.Call(snapshot, uintptr(unsafe.Pointer(&entry)))
	for found != 0 {
		if entry.ownerProcessID == processID {
			thread, _, callErr := openThread.Call(threadSuspendResume, 0, uintptr(entry.threadID))
			if thread == 0 {
				return nonzeroCallError(callErr)
			}
			resumed, _, callErr := resumeThread.Call(thread)
			syscall.CloseHandle(syscall.Handle(thread))
			if uint32(resumed) == ^uint32(0) {
				return nonzeroCallError(callErr)
			}
			return nil
		}
		found, _, callErr = thread32Next.Call(snapshot, uintptr(unsafe.Pointer(&entry)))
	}
	if callErr != syscall.ERROR_NO_MORE_FILES {
		return nonzeroCallError(callErr)
	}
	return errors.New("spawned process primary thread was unavailable")
}

func nonzeroCallError(err error) error {
	if err == nil || err == syscall.Errno(0) {
		return syscall.EINVAL
	}
	return err
}

type threadEntry32 struct {
	size           uint32
	usageCount     uint32
	threadID       uint32
	ownerProcessID uint32
	basePriority   int32
	priorityDelta  int32
	flags          uint32
}

type jobObjectBasicLimitInformation struct {
	perProcessUserTimeLimit int64
	perJobUserTimeLimit     int64
	limitFlags              uint32
	minimumWorkingSetSize   uintptr
	maximumWorkingSetSize   uintptr
	activeProcessLimit      uint32
	affinity                uintptr
	priorityClass           uint32
	schedulingClass         uint32
}

type ioCounters struct {
	readOperationCount  uint64
	writeOperationCount uint64
	otherOperationCount uint64
	readTransferCount   uint64
	writeTransferCount  uint64
	otherTransferCount  uint64
}

type jobObjectExtendedLimitInformation struct {
	basicLimitInformation jobObjectBasicLimitInformation
	ioInfo                ioCounters
	processMemoryLimit    uintptr
	jobMemoryLimit        uintptr
	peakProcessMemoryUsed uintptr
	peakJobMemoryUsed     uintptr
}
