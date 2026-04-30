namespace Kashot;

static class Program
{
    [STAThread]
    static void Main()
    {
        using var mutex = new System.Threading.Mutex(true, "Kashot.SingleInstance.Mutex", out var created);
        if (!created) return;

        ApplicationConfiguration.Initialize();
        Application.Run(new TrayContext());
    }
}
