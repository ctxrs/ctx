export const CLI_INSTALL_POWERSHELL_PATH_TYPES = `if ($null -eq ("CtxInstallerPathGuard" -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
using Microsoft.Win32.SafeHandles;

internal static class CtxInstallerNativePath
{
    internal const uint FILE_READ_ATTRIBUTES = 0x00000080;
    internal const uint GENERIC_WRITE = 0x40000000;
    internal const uint READ_CONTROL = 0x00020000;
    internal const uint WRITE_DAC = 0x00040000;
    internal const uint WRITE_OWNER = 0x00080000;
    internal const uint FILE_SHARE_READ = 0x00000001;
    internal const uint FILE_SHARE_WRITE = 0x00000002;
    internal const uint OPEN_EXISTING = 3;
    internal const uint OPEN_ALWAYS = 4;
    internal const uint FILE_ATTRIBUTE_DIRECTORY = 0x00000010;
    internal const uint FILE_ATTRIBUTE_REPARSE_POINT = 0x00000400;
    internal const uint FILE_FLAG_BACKUP_SEMANTICS = 0x02000000;
    internal const uint FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000;

    [StructLayout(LayoutKind.Sequential)]
    internal struct FileTime
    {
        internal uint Low;
        internal uint High;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct ByHandleFileInformation
    {
        internal uint FileAttributes;
        internal FileTime CreationTime;
        internal FileTime LastAccessTime;
        internal FileTime LastWriteTime;
        internal uint VolumeSerialNumber;
        internal uint FileSizeHigh;
        internal uint FileSizeLow;
        internal uint NumberOfLinks;
        internal uint FileIndexHigh;
        internal uint FileIndexLow;
    }

    internal sealed class FileIdentity
    {
        internal readonly uint VolumeSerialNumber;
        internal readonly ulong FileIndex;
        internal readonly string FinalPath;

        internal FileIdentity(uint volumeSerialNumber, ulong fileIndex, string finalPath)
        {
            VolumeSerialNumber = volumeSerialNumber;
            FileIndex = fileIndex;
            FinalPath = finalPath;
        }

        internal bool Equals(FileIdentity other)
        {
            return other != null &&
                VolumeSerialNumber == other.VolumeSerialNumber &&
                FileIndex == other.FileIndex &&
                String.Equals(FinalPath, other.FinalPath, StringComparison.OrdinalIgnoreCase);
        }
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(
        string fileName,
        uint desiredAccess,
        uint shareMode,
        IntPtr securityAttributes,
        uint creationDisposition,
        uint flagsAndAttributes,
        IntPtr templateFile
    );

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetFileInformationByHandle(
        SafeFileHandle file,
        out ByHandleFileInformation information
    );

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetFinalPathNameByHandleW(
        SafeFileHandle file,
        StringBuilder path,
        uint pathLength,
        uint flags
    );

    internal static SafeFileHandle Open(
        string path,
        uint desiredAccess,
        uint creationDisposition,
        uint shareMode
    )
    {
        SafeFileHandle handle = CreateFileW(
            path,
            desiredAccess,
            shareMode,
            IntPtr.Zero,
            creationDisposition,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            IntPtr.Zero
        );
        if (handle.IsInvalid)
        {
            int error = Marshal.GetLastWin32Error();
            handle.Dispose();
            throw new Win32Exception(error, "could not open managed installer path: " + path);
        }
        return handle;
    }

    internal static bool TryOpenExisting(string path, out SafeFileHandle handle)
    {
        handle = CreateFileW(
            path,
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            IntPtr.Zero,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            IntPtr.Zero
        );
        if (!handle.IsInvalid)
        {
            return true;
        }

        int error = Marshal.GetLastWin32Error();
        handle.Dispose();
        handle = null;
        if (error == 2 || error == 3)
        {
            return false;
        }
        throw new Win32Exception(error, "could not inspect managed installer path: " + path);
    }

    internal static FileIdentity Inspect(
        SafeFileHandle handle,
        string path,
        bool expectDirectory,
        bool destinationLeaf
    )
    {
        ByHandleFileInformation information;
        if (!GetFileInformationByHandle(handle, out information))
        {
            throw new Win32Exception(
                Marshal.GetLastWin32Error(),
                "could not inspect managed installer handle: " + path
            );
        }
        if ((information.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
        {
            string subject = destinationLeaf ? "destination" : "path";
            throw new InvalidOperationException(
                "ctx install " + subject + " must not contain reparse points: " + path
            );
        }

        bool isDirectory = (information.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0;
        if (isDirectory != expectDirectory)
        {
            string expected = expectDirectory ? "a directory" : "a regular file";
            throw new InvalidOperationException(
                "ctx install path must be " + expected + ": " + path
            );
        }
        if (destinationLeaf && information.NumberOfLinks > 1)
        {
            throw new InvalidOperationException(
                "ctx install destination must not be a hard link: " + path
            );
        }

        uint required = GetFinalPathNameByHandleW(handle, null, 0, 0);
        if (required == 0)
        {
            throw new Win32Exception(
                Marshal.GetLastWin32Error(),
                "could not resolve managed installer handle: " + path
            );
        }
        StringBuilder finalPath = new StringBuilder(checked((int)required + 1));
        uint written = GetFinalPathNameByHandleW(
            handle,
            finalPath,
            checked((uint)finalPath.Capacity),
            0
        );
        if (written == 0 || written >= (uint)finalPath.Capacity)
        {
            throw new Win32Exception(
                Marshal.GetLastWin32Error(),
                "could not resolve managed installer handle: " + path
            );
        }

        ulong fileIndex =
            ((ulong)information.FileIndexHigh << 32) | information.FileIndexLow;
        return new FileIdentity(
            information.VolumeSerialNumber,
            fileIndex,
            finalPath.ToString()
        );
    }

    internal static void AssertSame(
        FileIdentity expected,
        FileIdentity actual,
        string path
    )
    {
        if (!expected.Equals(actual))
        {
            throw new InvalidOperationException(
                "ctx install path changed during installation: " + path
            );
        }
    }

    internal static List<string> DirectoryChain(string path)
    {
        string fullPath = Path.GetFullPath(path);
        List<string> reverse = new List<string>();
        DirectoryInfo current = new DirectoryInfo(fullPath);
        while (current != null)
        {
            reverse.Add(current.FullName);
            current = current.Parent;
        }
        reverse.Reverse();
        return reverse;
    }
}

public static class CtxInstallerAcl
{
    private const uint SDDL_REVISION_1 = 1;
    private const int SE_FILE_OBJECT = 1;
    private const uint OWNER_SECURITY_INFORMATION = 0x00000001;
    private const uint DACL_SECURITY_INFORMATION = 0x00000004;
    private const uint PROTECTED_DACL_SECURITY_INFORMATION = 0x80000000;

    [DllImport(
        "advapi32.dll",
        EntryPoint = "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        CharSet = CharSet.Unicode,
        SetLastError = true
    )]
    private static extern bool ConvertSecurityDescriptor(
        string stringSecurityDescriptor,
        uint stringSecurityDescriptorRevision,
        out IntPtr securityDescriptor,
        out uint securityDescriptorSize
    );

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool GetSecurityDescriptorOwner(
        IntPtr securityDescriptor,
        out IntPtr owner,
        out bool ownerDefaulted
    );

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool GetSecurityDescriptorDacl(
        IntPtr securityDescriptor,
        out bool daclPresent,
        out IntPtr dacl,
        out bool daclDefaulted
    );

    [DllImport("advapi32.dll")]
    private static extern uint SetSecurityInfo(
        SafeFileHandle handle,
        int objectType,
        uint securityInformation,
        IntPtr owner,
        IntPtr group,
        IntPtr dacl,
        IntPtr sacl
    );

    [DllImport("kernel32.dll")]
    private static extern IntPtr LocalFree(IntPtr memory);

    public static void Protect(string path, bool directory, string currentSid)
    {
        string fullPath = Path.GetFullPath(path);
        SafeFileHandle handle = null;
        try
        {
            handle = CtxInstallerNativePath.Open(
                fullPath,
                CtxInstallerNativePath.FILE_READ_ATTRIBUTES |
                    CtxInstallerNativePath.READ_CONTROL |
                    CtxInstallerNativePath.WRITE_DAC |
                    CtxInstallerNativePath.WRITE_OWNER,
                CtxInstallerNativePath.OPEN_EXISTING,
                CtxInstallerNativePath.FILE_SHARE_READ |
                    CtxInstallerNativePath.FILE_SHARE_WRITE
            );
            Protect(handle, fullPath, directory, currentSid);
        }
        finally
        {
            if (handle != null)
            {
                handle.Dispose();
            }
        }
    }

    internal static void Protect(
        SafeFileHandle handle,
        string path,
        bool directory,
        string currentSid
    )
    {
        string fullPath = Path.GetFullPath(path);
        string canonicalCurrentSid = new SecurityIdentifier(currentSid).Value;
        string inheritance = directory ? "OICI" : "";
        string descriptor =
            "O:" + canonicalCurrentSid +
            "D:P" +
            "(A;" + inheritance + ";FA;;;" + canonicalCurrentSid + ")" +
            "(A;" + inheritance + ";FA;;;S-1-5-18)";

        IntPtr securityDescriptor = IntPtr.Zero;
        try
        {
            CtxInstallerNativePath.Inspect(
                handle,
                fullPath,
                directory,
                !directory
            );

            uint securityDescriptorSize;
            if (!ConvertSecurityDescriptor(
                descriptor,
                SDDL_REVISION_1,
                out securityDescriptor,
                out securityDescriptorSize
            ))
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "could not construct exact managed installer ACL: " + fullPath
                );
            }

            IntPtr owner;
            bool ownerDefaulted;
            if (!GetSecurityDescriptorOwner(
                securityDescriptor,
                out owner,
                out ownerDefaulted
            ) || owner == IntPtr.Zero)
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "could not read managed installer ACL owner: " + fullPath
                );
            }

            bool daclPresent;
            IntPtr dacl;
            bool daclDefaulted;
            if (!GetSecurityDescriptorDacl(
                securityDescriptor,
                out daclPresent,
                out dacl,
                out daclDefaulted
            ) || !daclPresent || dacl == IntPtr.Zero)
            {
                throw new Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "could not read exact managed installer DACL: " + fullPath
                );
            }

            uint result = SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION |
                    DACL_SECURITY_INFORMATION |
                    PROTECTED_DACL_SECURITY_INFORMATION,
                owner,
                IntPtr.Zero,
                dacl,
                IntPtr.Zero
            );
            if (result != 0)
            {
                throw new Win32Exception(
                    checked((int)result),
                    "could not apply exact managed installer ACL: " + fullPath
                );
            }
        }
        finally
        {
            if (securityDescriptor != IntPtr.Zero)
            {
                LocalFree(securityDescriptor);
            }
        }
    }
}

public sealed class CtxInstallerPathGuard : IDisposable
{
    private sealed class Entry
    {
        internal readonly string Path;
        internal readonly bool Directory;
        internal readonly bool DestinationLeaf;
        internal readonly SafeFileHandle Handle;
        internal readonly CtxInstallerNativePath.FileIdentity Identity;

        internal Entry(
            string path,
            bool directory,
            bool destinationLeaf,
            SafeFileHandle handle,
            CtxInstallerNativePath.FileIdentity identity
        )
        {
            Path = path;
            Directory = directory;
            DestinationLeaf = destinationLeaf;
            Handle = handle;
            Identity = identity;
        }
    }

    private readonly List<Entry> entries;
    public bool LeafExists { get; private set; }

    private CtxInstallerPathGuard(List<Entry> entries, bool leafExists)
    {
        this.entries = entries;
        LeafExists = leafExists;
    }

    public static CtxInstallerPathGuard AcquireDirectory(string path, bool requireLeaf)
    {
        List<string> chain = CtxInstallerNativePath.DirectoryChain(path);
        List<Entry> entries = new List<Entry>();
        bool leafExists = true;
        try
        {
            for (int index = 0; index < chain.Count; index++)
            {
                string current = chain[index];
                SafeFileHandle handle;
                if (!CtxInstallerNativePath.TryOpenExisting(current, out handle))
                {
                    leafExists = false;
                    break;
                }
                CtxInstallerNativePath.FileIdentity identity =
                    CtxInstallerNativePath.Inspect(handle, current, true, false);
                entries.Add(new Entry(current, true, false, handle, identity));
            }
            if (requireLeaf && !leafExists)
            {
                throw new InvalidOperationException(
                    "ctx install directory does not exist: " + Path.GetFullPath(path)
                );
            }
            return new CtxInstallerPathGuard(entries, leafExists);
        }
        catch
        {
            foreach (Entry entry in entries)
            {
                entry.Handle.Dispose();
            }
            throw;
        }
    }

    public static CtxInstallerPathGuard AcquireLeaf(string path)
    {
        string fullPath = Path.GetFullPath(path);
        string parent = Path.GetDirectoryName(fullPath);
        if (String.IsNullOrEmpty(parent))
        {
            throw new InvalidOperationException(
                "ctx install destination has no parent directory: " + fullPath
            );
        }

        List<string> chain = CtxInstallerNativePath.DirectoryChain(parent);
        List<Entry> entries = new List<Entry>();
        try
        {
            foreach (string current in chain)
            {
                SafeFileHandle directoryHandle;
                if (!CtxInstallerNativePath.TryOpenExisting(current, out directoryHandle))
                {
                    throw new InvalidOperationException(
                        "ctx install destination parent does not exist: " + current
                    );
                }
                CtxInstallerNativePath.FileIdentity directoryIdentity =
                    CtxInstallerNativePath.Inspect(
                        directoryHandle,
                        current,
                        true,
                        false
                    );
                entries.Add(new Entry(
                    current,
                    true,
                    false,
                    directoryHandle,
                    directoryIdentity
                ));
            }

            SafeFileHandle leafHandle;
            bool leafExists =
                CtxInstallerNativePath.TryOpenExisting(fullPath, out leafHandle);
            if (leafExists)
            {
                CtxInstallerNativePath.FileIdentity leafIdentity =
                    CtxInstallerNativePath.Inspect(
                        leafHandle,
                        fullPath,
                        false,
                        true
                    );
                entries.Add(new Entry(
                    fullPath,
                    false,
                    true,
                    leafHandle,
                    leafIdentity
                ));
            }
            return new CtxInstallerPathGuard(entries, leafExists);
        }
        catch
        {
            foreach (Entry entry in entries)
            {
                entry.Handle.Dispose();
            }
            throw;
        }
    }

    public void AssertCanonical()
    {
        if (!LeafExists || entries.Count == 0)
            throw new InvalidOperationException("recovery path must already exist");
        Entry leaf = entries[entries.Count - 1];
        string requested = Path.GetFullPath(leaf.Path);
        string actual = leaf.Identity.FinalPath;
        if (requested.StartsWith(@"\\\\?\\", StringComparison.Ordinal)) requested = requested.Substring(4);
        if (actual.StartsWith(@"\\\\?\\", StringComparison.Ordinal)) actual = actual.Substring(4);
        if (!String.Equals(requested, actual, StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException("recovery path must be canonical: " + requested);
        AssertUnchanged();
    }

    public void AssertUnchanged()
    {
        foreach (Entry entry in entries)
        {
            SafeFileHandle currentHandle = null;
            try
            {
                if (!CtxInstallerNativePath.TryOpenExisting(entry.Path, out currentHandle))
                {
                    throw new InvalidOperationException(
                        "ctx install path disappeared during installation: " + entry.Path
                    );
                }
                CtxInstallerNativePath.FileIdentity currentIdentity =
                    CtxInstallerNativePath.Inspect(
                        currentHandle,
                        entry.Path,
                        entry.Directory,
                        entry.DestinationLeaf
                    );
                CtxInstallerNativePath.AssertSame(
                    entry.Identity,
                    currentIdentity,
                    entry.Path
                );
            }
            finally
            {
                if (currentHandle != null)
                {
                    currentHandle.Dispose();
                }
            }
        }
    }

    public void Dispose()
    {
        foreach (Entry entry in entries)
        {
            entry.Handle.Dispose();
        }
        entries.Clear();
    }
}

'@
}
`;

export const CLI_INSTALL_POWERSHELL_PLATFORM = `${CLI_INSTALL_POWERSHELL_PATH_TYPES}
$initialInstallPathGuard = $null
try {
    $initialInstallPathGuard = [CtxInstallerPathGuard]::AcquireDirectory($BinDir, $false)
    $initialInstallPathGuard.AssertUnchanged()
} finally {
    if ($null -ne $initialInstallPathGuard) {
        $initialInstallPathGuard.Dispose()
    }
}

`;
