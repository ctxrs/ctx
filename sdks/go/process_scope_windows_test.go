//go:build windows

package ctxagenthistory

const processQueryLimitedInformation = 0x1000

func processAlive(pid int) bool {
	handle, _, _ := openProcess.Call(
		processQueryLimitedInformation,
		0,
		uintptr(uint32(pid)),
	)
	if handle == 0 {
		return false
	}
	closeHandle.Call(handle)
	return true
}
