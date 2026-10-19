import com.fasterxml.jackson.core.StreamReadConstraints;

/** Starts the pinned solver with finite JSON admission limits for generated models. */
public final class LoomApalache {
    private LoomApalache() {}

    public static void main(String[] args) throws Exception {
        StreamReadConstraints.overrideDefaultStreamReadConstraints(
            StreamReadConstraints.builder()
                .maxStringLength(64 * 1024 * 1024)
                .maxDocumentLength(96L * 1024 * 1024)
                .build());
        at.forsyte.apalache.tla.Tool.main(args);
    }
}
