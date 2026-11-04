// Test helpers for test-installer.ps1.
// ShellMenu drives the same context-menu interface File Explorer uses, so tests can inspect the
// real menu for a selection of files and click its entries. AppWindow captures and closes dialogs.

using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;

public static class ShellMenu
{
    [ComImport, Guid("000214E6-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IShellFolder
    {
        void ParseDisplayName(IntPtr hwnd, IntPtr bindContext, [MarshalAs(UnmanagedType.LPWStr)] string name, out uint eaten, out IntPtr pidl, ref uint attributes);
        void EnumObjects(IntPtr hwnd, int flags, out IntPtr list);
        void BindToObject(IntPtr pidl, IntPtr bindContext, [In] ref Guid iid, out IntPtr result);
        void BindToStorage(IntPtr pidl, IntPtr bindContext, [In] ref Guid iid, out IntPtr result);
        [PreserveSig] int CompareIDs(IntPtr lParam, IntPtr pidl1, IntPtr pidl2);
        void CreateViewObject(IntPtr hwnd, [In] ref Guid iid, out IntPtr result);
        void GetAttributesOf(uint count, [In, MarshalAs(UnmanagedType.LPArray)] IntPtr[] pidls, ref uint attributes);
        void GetUIObjectOf(IntPtr hwnd, uint count, [In, MarshalAs(UnmanagedType.LPArray)] IntPtr[] pidls, [In] ref Guid iid, IntPtr reserved, [MarshalAs(UnmanagedType.IUnknown)] out object result);
    }

    [ComImport, Guid("000214e4-0000-0000-c000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IContextMenu
    {
        [PreserveSig] int QueryContextMenu(IntPtr menu, uint index, uint first, uint last, uint flags);
        void InvokeCommand(ref CommandInfo info);
        void GetCommandString(UIntPtr command, uint type, IntPtr reserved, StringBuilder name, uint size);
    }

    [ComImport, Guid("000214f4-0000-0000-c000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IContextMenu2
    {
        [PreserveSig] int QueryContextMenu(IntPtr menu, uint index, uint first, uint last, uint flags);
        void InvokeCommand(ref CommandInfo info);
        void GetCommandString(UIntPtr command, uint type, IntPtr reserved, StringBuilder name, uint size);
        void HandleMenuMsg(uint message, IntPtr wParam, IntPtr lParam);
    }

    [StructLayout(LayoutKind.Sequential)]
    struct CommandInfo
    {
        public int Size;
        public int Mask;
        public IntPtr Window;
        public IntPtr Verb;
        public IntPtr Parameters;
        public IntPtr Directory;
        public int Show;
        public int HotKey;
        public IntPtr Icon;
    }

    [DllImport("shell32.dll")] static extern int SHGetDesktopFolder(out IShellFolder folder);
    [DllImport("shell32.dll", CharSet = CharSet.Unicode)] static extern int SHParseDisplayName(string name, IntPtr bindContext, out IntPtr pidl, uint request, out uint attributes);
    [DllImport("shell32.dll")] static extern int SHBindToParent(IntPtr pidl, [In] ref Guid iid, [MarshalAs(UnmanagedType.IUnknown)] out object folder, out IntPtr child);
    [DllImport("user32.dll")] static extern IntPtr CreatePopupMenu();
    [DllImport("user32.dll")] static extern bool DestroyMenu(IntPtr menu);
    [DllImport("user32.dll")] static extern int GetMenuItemCount(IntPtr menu);
    [DllImport("user32.dll")] static extern IntPtr GetSubMenu(IntPtr menu, int position);
    [DllImport("user32.dll")] static extern uint GetMenuItemID(IntPtr menu, int position);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetMenuString(IntPtr menu, uint item, StringBuilder text, int size, uint flags);

    const uint CMF_NORMAL = 0;
    const uint MF_BYPOSITION = 0x400;
    const uint WM_INITMENUPOPUP = 0x0117;
    const int SW_SHOWNORMAL = 1;
    const uint FIRST_ID = 1;
    const uint LAST_ID = 0x7FFF;

    static string Clean(string label) { return label.Replace("&", ""); }

    static IContextMenu MenuFor(string[] files, out IntPtr menu)
    {
        Guid folderId = new Guid("000214E6-0000-0000-C000-000000000046");
        Guid menuId = new Guid("000214e4-0000-0000-c000-000000000046");
        IntPtr[] children = new IntPtr[files.Length];
        object parentObject = null;
        for (int i = 0; i < files.Length; i++)
        {
            IntPtr pidl;
            uint attributes;
            int result = SHParseDisplayName(files[i], IntPtr.Zero, out pidl, 0, out attributes);
            if (result != 0) throw new IOException("Could not resolve " + files[i]);
            result = SHBindToParent(pidl, ref folderId, out parentObject, out children[i]);
            if (result != 0) throw new IOException("Could not bind " + files[i]);
        }
        object uiObject;
        ((IShellFolder)parentObject).GetUIObjectOf(IntPtr.Zero, (uint)children.Length, children, ref menuId, IntPtr.Zero, out uiObject);
        IContextMenu contextMenu = (IContextMenu)uiObject;
        menu = CreatePopupMenu();
        contextMenu.QueryContextMenu(menu, 0, FIRST_ID, LAST_ID, CMF_NORMAL);
        return contextMenu;
    }

    static void Collect(IContextMenu contextMenu, IntPtr menu, string prefix, Dictionary<string, uint> found)
    {
        IContextMenu2 populator = contextMenu as IContextMenu2;
        int count = GetMenuItemCount(menu);
        for (int i = 0; i < count; i++)
        {
            StringBuilder text = new StringBuilder(256);
            GetMenuString(menu, (uint)i, text, text.Capacity, MF_BYPOSITION);
            string label = Clean(text.ToString());
            if (label.Length == 0) continue;
            string path = prefix + label;
            IntPtr sub = GetSubMenu(menu, i);
            if (sub != IntPtr.Zero)
            {
                if (populator != null)
                {
                    try { populator.HandleMenuMsg(WM_INITMENUPOPUP, sub, (IntPtr)i); } catch (Exception) { }
                }
                Collect(contextMenu, sub, path + " > ", found);
                found[path] = 0;
            }
            else
            {
                found[path] = GetMenuItemID(menu, i);
            }
        }
    }

    /// Lists the labels of the context menu for a selection, with submenu items as "Parent > Child".
    public static string[] Items(string[] files)
    {
        IntPtr menu;
        IContextMenu contextMenu = MenuFor(files, out menu);
        Dictionary<string, uint> found = new Dictionary<string, uint>();
        Collect(contextMenu, menu, "", found);
        DestroyMenu(menu);
        return found.Keys.ToArray();
    }

    /// Clicks an entry of the context menu for a selection.
    public static void Click(string[] files, string label)
    {
        IntPtr menu;
        IContextMenu contextMenu = MenuFor(files, out menu);
        Dictionary<string, uint> found = new Dictionary<string, uint>();
        Collect(contextMenu, menu, "", found);
        uint id;
        if (!found.TryGetValue(label, out id) || id == 0) throw new ArgumentException("No such menu item: " + label);
        CommandInfo info = new CommandInfo();
        info.Size = Marshal.SizeOf(typeof(CommandInfo));
        info.Verb = (IntPtr)(id - FIRST_ID);
        info.Show = SW_SHOWNORMAL;
        contextMenu.InvokeCommand(ref info);
        DestroyMenu(menu);
    }
}

public static class AppWindow
{
    delegate bool ChildVisitor(IntPtr child, IntPtr data);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr window, ChildVisitor visit, IntPtr data);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr window, StringBuilder text, int size);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential)]
    struct Rect { public int Left, Top, Right, Bottom; }

    const uint PW_RENDERFULLCONTENT = 2;
    const uint WM_CLOSE = 0x0010;

    /// Whether a dialog has a button with this label. The progress dialog is the one with Cancel.
    public static bool HasButton(IntPtr window, string label)
    {
        bool found = false;
        EnumChildWindows(window, delegate (IntPtr child, IntPtr data)
        {
            StringBuilder text = new StringBuilder(64);
            GetWindowText(child, text, text.Capacity);
            if (text.ToString().Replace("&", "") == label) found = true;
            return true;
        }, IntPtr.Zero);
        return found;
    }

    /// Saves an image of just this window, whatever is on top of it.
    public static void Capture(IntPtr window, string path)
    {
        Rect rect;
        GetWindowRect(window, out rect);
        using (Bitmap image = new Bitmap(rect.Right - rect.Left, rect.Bottom - rect.Top))
        {
            using (Graphics graphics = Graphics.FromImage(image))
            {
                IntPtr dc = graphics.GetHdc();
                PrintWindow(window, dc, PW_RENDERFULLCONTENT);
                graphics.ReleaseHdc(dc);
            }
            image.Save(path, ImageFormat.Png);
        }
    }

    public static void Close(IntPtr window)
    {
        PostMessage(window, WM_CLOSE, IntPtr.Zero, IntPtr.Zero);
    }
}
