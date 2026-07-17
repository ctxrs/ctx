//go:build linux

package ctxagenthistory

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"syscall"
	"testing"
)

func processAlive(pid int) bool {
	stat, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", pid))
	if errors.Is(err, os.ErrNotExist) {
		return false
	}
	if err == nil {
		if state, ok := linuxProcessState(stat); ok && state == 'Z' {
			// Container PID 1 may not reap promptly. A zombie has exited and can no
			// longer execute or retain handles, so it is not a surviving process.
			return false
		}
	}
	err = syscall.Kill(pid, 0)
	return err == nil || err == syscall.EPERM
}

func linuxProcessState(stat []byte) (byte, bool) {
	closingParenthesis := bytes.LastIndexByte(stat, ')')
	stateOffset := closingParenthesis + 2
	if closingParenthesis < 0 || stateOffset >= len(stat) {
		return 0, false
	}
	return stat[stateOffset], true
}

func TestLinuxProcessStateDistinguishesZombieFromLiveStates(t *testing.T) {
	for _, test := range []struct {
		state byte
		alive bool
	}{
		{state: 'R', alive: true},
		{state: 'S', alive: true},
		{state: 'D', alive: true},
		{state: 'T', alive: true},
		{state: 'Z', alive: false},
	} {
		stat := []byte(fmt.Sprintf("123 (ctx fixture) %c 1 2 3", test.state))
		state, ok := linuxProcessState(stat)
		if !ok || (state != 'Z') != test.alive {
			t.Fatalf("state %c classified incorrectly: parsed=%c ok=%t", test.state, state, ok)
		}
	}
}
