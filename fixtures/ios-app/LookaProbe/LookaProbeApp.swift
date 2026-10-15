import SwiftUI

@main
struct LookaProbeApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}

struct ContentView: View {
    @State private var taps = 0

    var body: some View {
        VStack(spacing: 24) {
            Text("Looka Probe")
                .font(.largeTitle.bold())
            Text("Taps: \(taps)")
            Button("Tap") { taps += 1 }
                .buttonStyle(.borderedProminent)
        }
        .padding()
        .onAppear { print("LOOKA_APP_READY") }
    }
}
