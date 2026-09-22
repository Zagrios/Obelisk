import Network

/// Retry deferred work only when connectivity returns after an observed outage.
@MainActor
final class NetworkReturnObservation {
    private let monitor = NWPathMonitor()
    private let action: @MainActor () -> Void
    private var wasSatisfied: Bool?

    init(observeSystem: Bool = true, action: @escaping @MainActor () -> Void) {
        self.action = action
        if observeSystem {
            monitor.pathUpdateHandler = { [weak self] path in
                let satisfied = path.status == .satisfied
                Task { @MainActor [weak self] in self?.receive(isSatisfied: satisfied) }
            }
            monitor.start(queue: DispatchQueue(label: "com.zagrios.forge.connectivity"))
        }
    }

    deinit { monitor.cancel() }

    func receive(isSatisfied: Bool) {
        let returned = wasSatisfied == false && isSatisfied
        wasSatisfied = isSatisfied
        if returned { action() }
    }
}
