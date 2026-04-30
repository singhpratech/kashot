// OcrService.cs
// OCR backend for Kashot, implemented using the Tesseract NuGet package
// (managed wrapper around bundled native leptonica + tesseract DLLs).
//
// Why Tesseract and not Windows.Media.Ocr:
//   - Windows.Media.Ocr would force a Windows10-versioned TFM
//     (e.g. net8.0-windows10.0.19041.0), affecting the whole project.
//   - The Tesseract NuGet ships native x86/x64 binaries via its .targets,
//     which copy automatically to the output directory. That keeps the
//     project's TargetFramework as plain net8.0-windows.
//
// Trained data:
//   The Tesseract package does NOT ship language data. We lazily download
//   `eng.traineddata` from the official tessdata_fast repository on first
//   use and cache it under %APPDATA%\Kashot\tessdata\. IsAvailable reports
//   true once the file is on disk; ExtractTextAsync will attempt to fetch
//   it if missing, so the first call may incur a one-time network round-trip.
//
// Threading:
//   TesseractEngine is not thread-safe. We serialize calls with a
//   SemaphoreSlim and lazily construct/keep one engine for the process.

using System;
using System.IO;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;
using Tesseract;

namespace Kashot;

public static class OcrService
{
    private const string TessdataFolderName = "tessdata";
    private const string LanguageCode = "eng";
    private const string TrainedDataFileName = "eng.traineddata";

    // tessdata_fast gives a smaller (~1.5 MB) model that is plenty for
    // screenshot text. Switch to tessdata or tessdata_best for higher
    // accuracy at the cost of larger downloads.
    private const string TrainedDataUrl =
        "https://github.com/tesseract-ocr/tessdata_fast/raw/main/eng.traineddata";

    private static readonly SemaphoreSlim _gate = new(1, 1);
    private static TesseractEngine? _engine;
    private static readonly Lazy<HttpClient> _http = new(() =>
    {
        var c = new HttpClient { Timeout = TimeSpan.FromSeconds(60) };
        c.DefaultRequestHeaders.UserAgent.ParseAdd("Kashot-OCR/1.0");
        return c;
    });

    /// <summary>
    /// True if OCR trained data is already on disk and ready to use.
    /// A false result does not mean OCR is impossible — ExtractTextAsync
    /// will attempt to download trained data on demand.
    /// </summary>
    public static bool IsAvailable
    {
        get
        {
            try
            {
                return File.Exists(Path.Combine(GetTessdataDirectory(), TrainedDataFileName));
            }
            catch
            {
                return false;
            }
        }
    }

    /// <summary>
    /// Extract text from a bitmap. Returns the extracted text trimmed,
    /// or an empty string if no text was recognized. Throws on hard
    /// failure (caller wraps in try/catch).
    /// </summary>
    public static async Task<string> ExtractTextAsync(System.Drawing.Bitmap image)
    {
        if (image is null) throw new ArgumentNullException(nameof(image));

        // Snapshot the bitmap to PNG bytes on the calling thread; the
        // Bitmap is owned by the caller and may be disposed before our
        // background work runs.
        byte[] pngBytes;
        using (var ms = new MemoryStream())
        {
            image.Save(ms, System.Drawing.Imaging.ImageFormat.Png);
            pngBytes = ms.ToArray();
        }

        return await Task.Run(async () =>
        {
            var dataDir = GetTessdataDirectory();
            await EnsureTrainedDataAsync(dataDir).ConfigureAwait(false);

            await _gate.WaitAsync().ConfigureAwait(false);
            try
            {
                var engine = GetOrCreateEngine(dataDir);
                using var pix = Pix.LoadFromMemory(pngBytes);
                using var page = engine.Process(pix);
                return (page.GetText() ?? string.Empty).Trim();
            }
            finally
            {
                _gate.Release();
            }
        }).ConfigureAwait(false);
    }

    private static TesseractEngine GetOrCreateEngine(string dataDir)
    {
        if (_engine is not null) return _engine;
        // Default mode picks up the bundled language model; LSTM is the default
        // engine in tesseract 5.x and works well for screenshot text.
        _engine = new TesseractEngine(dataDir, LanguageCode, EngineMode.Default);
        return _engine;
    }

    private static string GetTessdataDirectory()
    {
        var appData = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        var dir = Path.Combine(appData, "Kashot", TessdataFolderName);
        Directory.CreateDirectory(dir);
        return dir;
    }

    private static async Task EnsureTrainedDataAsync(string dataDir)
    {
        var target = Path.Combine(dataDir, TrainedDataFileName);
        if (File.Exists(target)) return;

        // Download to a temp file in the same directory, then atomic-rename.
        // Avoids leaving a half-written file if the process is killed mid-download.
        var temp = target + ".part";
        try
        {
            using (var resp = await _http.Value
                .GetAsync(TrainedDataUrl, HttpCompletionOption.ResponseHeadersRead)
                .ConfigureAwait(false))
            {
                resp.EnsureSuccessStatusCode();
                await using var src = await resp.Content.ReadAsStreamAsync().ConfigureAwait(false);
                await using var dst = new FileStream(
                    temp, FileMode.Create, FileAccess.Write, FileShare.None);
                await src.CopyToAsync(dst).ConfigureAwait(false);
            }

            if (File.Exists(target))
            {
                // Another caller won the race; discard our copy.
                File.Delete(temp);
                return;
            }

            File.Move(temp, target);
        }
        catch
        {
            try { if (File.Exists(temp)) File.Delete(temp); } catch { /* best-effort */ }
            throw;
        }
    }
}
