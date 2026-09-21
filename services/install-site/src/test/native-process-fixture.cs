using System;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Threading;

// A real native process boundary for both Windows PowerShell 5.1 and PowerShell 7.
public static class NativeProcessFixture
{
    public static int Main(string[] args)
    {
        Console.OutputEncoding = new UTF8Encoding(false);
        if (args.Length == 0) return 0;
        switch (args[0])
        {
            case "streams":
                Console.Out.Write("native stdout\n");
                Console.Error.Write("native stderr\n");
                return Int32.Parse(args[1]);
            case "arguments":
                for (int i = 1; i < args.Length; i++)
                    Console.WriteLine(Convert.ToBase64String(Encoding.UTF8.GetBytes(args[i])));
                return 0;
            case "large":
                for (int i = 0; i < 1024; i++)
                {
                    Console.Out.Write(new string('o', 1024));
                    Console.Error.Write(new string('e', 1024));
                }
                return 0;
            case "descendant":
                var start = new ProcessStartInfo {
                    FileName = Process.GetCurrentProcess().MainModule.FileName,
                    Arguments = "linger", UseShellExecute = false, CreateNoWindow = true
                };
                using (var child = Process.Start(start))
                    File.WriteAllText(args[1], child.Id.ToString());
                return 0;
            case "linger":
                Thread.Sleep(15000);
                return 0;
            default:
                return 2;
        }
    }
}
