import AVFoundation
import Speech

/// Hold-to-talk on the phone itself: the iOS recogniser, on-device where the phone supports it.
/// Nothing is sent to the Shirley server -- only the text that comes out.
@MainActor
final class SpeechInput {
    /// Chinese, not the system language: the UI is Chinese, and an English phone with a
    /// Chinese-speaking user would otherwise transcribe Chinese as English (same as the web UI).
    static let locale = Locale(identifier: "zh-CN")

    enum Failure: LocalizedError {
        case denied, noMicrophone, unavailable, failed

        var errorDescription: String? {
            switch self {
            case .denied: "麦克风或语音识别权限被拒绝。请在「设置 › Shirley」里允许麦克风和语音识别"
            case .noMicrophone: "找不到麦克风，请检查输入设备"
            case .unavailable: "这台手机现在用不了语音识别，请用文字输入"
            case .failed: "麦克风打不开，请再试一次"
            }
        }

        /// Voice cannot work here right now: switch to the keyboard.
        var fatal: Bool { self == .denied || self == .unavailable }
    }

    private let engine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var started: Date?
    private var latest = ""
    private var finished = false
    private var waiting: CheckedContinuation<String, Never>?
    private var onText: ((String) -> Void)?

    var running: Bool { started != nil }

    /// Seconds since the microphone opened.
    var elapsed: TimeInterval { started.map { Date().timeIntervalSince($0) } ?? 0 }

    /// The first press will raise a system prompt.
    static var willAsk: Bool {
        SFSpeechRecognizer.authorizationStatus() == .notDetermined
            || AVAudioApplication.shared.recordPermission == .undetermined
    }

    private static func authorize() async -> Bool {
        let speech: SFSpeechRecognizerAuthorizationStatus = await withCheckedContinuation { done in
            SFSpeechRecognizer.requestAuthorization { done.resume(returning: $0) }
        }
        guard speech == .authorized else { return false }
        return await AVAudioApplication.requestRecordPermission()
    }

    /// Open the microphone and start recognising; `onText` hears every partial result.
    func begin(onText: @escaping (String) -> Void) async throws {
        guard await Self.authorize() else { throw Failure.denied }
        guard let recognizer = SFSpeechRecognizer(locale: Self.locale), recognizer.isAvailable else {
            throw Failure.unavailable
        }
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.record, mode: .measurement, options: .duckOthers)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
        } catch {
            throw Failure.failed
        }

        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }

        let input = engine.inputNode
        guard Self.tap(input, into: request) else {
            deactivate()
            throw Failure.noMicrophone
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            deactivate()
            throw Failure.failed
        }

        self.request = request
        self.onText = onText
        latest = ""
        finished = false
        started = Date()
        task = Self.recognize(recognizer, request) { [weak self] text, done in
            self?.heard(text, done: done)
        }
    }

    /// Stop listening and wait (briefly) for the recogniser's last word.
    func finish() async -> String {
        stopAudio()
        request?.endAudio()
        let text: String
        if finished {
            text = latest
        } else {
            text = await withCheckedContinuation { done in
                waiting = done
                Task { [weak self] in
                    try? await Task.sleep(nanoseconds: 2_500_000_000)
                    self?.resume()
                }
            }
        }
        reset()
        return text
    }

    /// Released too soon, or the screen went away: drop everything.
    func cancel() {
        stopAudio()
        task?.cancel()
        resume()
        reset()
    }

    private func heard(_ text: String?, done: Bool) {
        if let text, !text.isEmpty {
            latest = text
            onText?(text)
        }
        if done {
            finished = true
            resume()
        }
    }

    private func resume() {
        waiting?.resume(returning: latest)
        waiting = nil
    }

    private func stopAudio() {
        if engine.isRunning { engine.stop() }
        engine.inputNode.removeTap(onBus: 0)
    }

    private func reset() {
        task = nil
        request = nil
        onText = nil
        started = nil
        deactivate()
    }

    private func deactivate() {
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    /// Outside the main actor: the tap runs on the audio thread.
    nonisolated private static func tap(_ input: AVAudioInputNode, into request: SFSpeechAudioBufferRecognitionRequest) -> Bool {
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { return false }
        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
            request.append(buffer)
        }
        return true
    }

    /// Outside the main actor: results arrive on the recogniser's queue and hop back here.
    nonisolated private static func recognize(
        _ recognizer: SFSpeechRecognizer,
        _ request: SFSpeechAudioBufferRecognitionRequest,
        deliver: @escaping @MainActor (String?, Bool) -> Void
    ) -> SFSpeechRecognitionTask {
        recognizer.recognitionTask(with: request) { result, error in
            let text = result?.bestTranscription.formattedString
            let done = (result?.isFinal ?? false) || error != nil
            Task { @MainActor in deliver(text, done) }
        }
    }
}
