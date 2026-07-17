package main

import (
	"context"
	"fmt"
	"os"

	"github.com/ctxrs/ctx/internal/cli"
)

var version = "dev"

func main() {
	app := cli.NewApp(os.Stdout, os.Stderr, cli.Dependencies{
		Version: version,
	})
	if err := app.Run(context.Background(), os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(cli.ExitCode(err))
	}
}
