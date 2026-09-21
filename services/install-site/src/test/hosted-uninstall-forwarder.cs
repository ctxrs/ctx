using System;
using System.Diagnostics;
using System.Reflection;
using System.Text;

// Transport only: the same task-owned Node fixture handles both platforms.
public static class HostedUninstallForwarder
{
    private static string Quote(string value)
    {
        var result = new StringBuilder("\"");
        int slashes = 0;
        foreach (char c in value)
        {
            if (c == '\\') { slashes++; continue; }
            result.Append('\\', c == '"' ? slashes * 2 + 1 : slashes);
            result.Append(c);
            slashes = 0;
        }
        return result.Append('\\', slashes * 2).Append('"').ToString();
    }

    public static int Main(string[] args)
    {
        try
        {
            string node = Environment.GetEnvironmentVariable("CTX_UNINSTALL_FIXTURE_NODE");
            string script = Environment.GetEnvironmentVariable("CTX_UNINSTALL_FIXTURE_SCRIPT");
            if (String.IsNullOrEmpty(node) || String.IsNullOrEmpty(script)) return 91;
            var arguments = new StringBuilder(Quote(script));
            arguments.Append(' ').Append(Quote(Assembly.GetEntryAssembly().Location));
            foreach (string arg in args) arguments.Append(' ').Append(Quote(arg));
            var start = new ProcessStartInfo(node, arguments.ToString());
            start.UseShellExecute = false;
            start.CreateNoWindow = true;
            start.RedirectStandardInput = true;
            using (Process child = Process.Start(start))
            {
                child.StandardInput.Close();
                if (!child.WaitForExit(10000))
                {
                    child.Kill();
                    child.WaitForExit(5000);
                    return 124;
                }
                return child.ExitCode;
            }
        }
        catch
        {
            Console.Error.WriteLine("hosted uninstall fixture forwarder failed");
            return 91;
        }
    }
}
