using System;
using System.Collections.Generic;
using System.IO;
using ScreenRecorderLib;

namespace Kashot;

/// <summary>
/// Wraps ScreenRecorderLib (Windows.Graphics.Capture + Media Foundation) to record
/// the primary display to MP4 (H.264 video + AAC audio) at 30 FPS. Mic and system
/// audio loopback are toggleable.
/// </summary>
public sealed class KashotRecorder : IDisposable
{
    private Recorder? _recorder;
    private string? _outputPath;
    private readonly object _gate = new();
    private bool _disposed;

    /// <summary>True while a recording is in progress (between Start and the
    /// Complete/Failed callback).</summary>
    public bool IsRecording { get; private set; }

    /// <summary>Fired with the output path when the recording successfully finishes.</summary>
    public event Action<string>? RecordingComplete;

    /// <summary>Fired with an error message when recording fails.</summary>
    public event Action<string>? RecordingFailed;

    /// <summary>Begin recording the primary display to MP4 at outputPath.</summary>
    public void Start(string outputPath, bool micEnabled, bool systemAudioEnabled)
    {
        if (_disposed) throw new ObjectDisposedException(nameof(KashotRecorder));
        if (string.IsNullOrWhiteSpace(outputPath))
            throw new ArgumentException("Output path required.", nameof(outputPath));

        lock (_gate)
        {
            if (IsRecording)
            {
                RaiseFailed("A recording is already in progress.");
                return;
            }

            try
            {
                var dir = Path.GetDirectoryName(Path.GetFullPath(outputPath));
                if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);

                _outputPath = outputPath;

                var primary = DisplayRecordingSource.MainMonitor;
                if (primary == null)
                {
                    RaiseFailed("No primary display available to record.");
                    return;
                }

                var sources = new List<RecordingSourceBase> { primary };

                var options = new RecorderOptions
                {
                    SourceOptions = new SourceOptions { RecordingSources = sources },
                    OutputOptions = new OutputOptions
                    {
                        RecorderMode = RecorderMode.Video,
                    },
                    VideoEncoderOptions = new VideoEncoderOptions
                    {
                        Encoder = new H264VideoEncoder
                        {
                            BitrateMode = H264BitrateControlMode.Quality,
                            EncoderProfile = H264Profile.Main,
                        },
                        Framerate = 30,
                        Quality = 70,
                        IsFixedFramerate = true,
                        IsHardwareEncodingEnabled = true,
                        IsMp4FastStartEnabled = true,
                    },
                    AudioOptions = new AudioOptions
                    {
                        // Only emit an audio track if at least one source is on,
                        // otherwise some players will choke on an empty AAC track.
                        IsAudioEnabled = micEnabled || systemAudioEnabled,
                        IsInputDeviceEnabled = micEnabled,
                        IsOutputDeviceEnabled = systemAudioEnabled,
                        Bitrate = AudioBitrate.bitrate_128kbps,
                        Channels = AudioChannels.Stereo,
                    },
                    MouseOptions = new MouseOptions
                    {
                        IsMousePointerEnabled = true,
                    },
                    LogOptions = new LogOptions
                    {
                        IsLogEnabled = false,
                    },
                };

                _recorder = Recorder.CreateRecorder(options);
                _recorder.OnRecordingComplete += OnLibComplete;
                _recorder.OnRecordingFailed += OnLibFailed;

                IsRecording = true;
                _recorder.Record(outputPath);
            }
            catch (Exception ex)
            {
                IsRecording = false;
                DisposeRecorder();
                RaiseFailed("Failed to start recording: " + ex.Message);
            }
        }
    }

    /// <summary>Stop recording. RecordingComplete fires when the file is finalized.</summary>
    public void Stop()
    {
        Recorder? r;
        lock (_gate)
        {
            if (!IsRecording || _recorder == null) return;
            r = _recorder;
        }
        try
        {
            r.Stop();
        }
        catch (Exception ex)
        {
            // Stop failures should still surface as a failure event so the UI
            // can reset its state.
            lock (_gate) IsRecording = false;
            RaiseFailed("Failed to stop recording: " + ex.Message);
        }
    }

    private void OnLibComplete(object? sender, RecordingCompleteEventArgs e)
    {
        string path;
        lock (_gate)
        {
            IsRecording = false;
            path = e?.FilePath ?? _outputPath ?? string.Empty;
            DisposeRecorder();
        }
        try { RecordingComplete?.Invoke(path); } catch { /* swallow handler errors */ }
    }

    private void OnLibFailed(object? sender, RecordingFailedEventArgs e)
    {
        string err = e?.Error ?? "Unknown recording error.";
        lock (_gate)
        {
            IsRecording = false;
            DisposeRecorder();
        }
        RaiseFailed(err);
    }

    private void RaiseFailed(string message)
    {
        try { RecordingFailed?.Invoke(message); } catch { /* swallow handler errors */ }
    }

    private void DisposeRecorder()
    {
        // Caller must hold _gate when invoking this.
        if (_recorder == null) return;
        try
        {
            _recorder.OnRecordingComplete -= OnLibComplete;
            _recorder.OnRecordingFailed -= OnLibFailed;
        }
        catch { /* ignore */ }
        try { (_recorder as IDisposable)?.Dispose(); } catch { /* ignore */ }
        _recorder = null;
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        lock (_gate)
        {
            if (IsRecording && _recorder != null)
            {
                try { _recorder.Stop(); } catch { /* ignore */ }
            }
            DisposeRecorder();
            IsRecording = false;
        }
    }
}
