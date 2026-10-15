# Runtime-only image. Build context must contain the cross-compiled static ELF,
# NOTICE, and licenses; never send the repository or private files as context.
FROM scratch
COPY jxpan-rust /app/jxpan-rust
COPY NOTICE /app/NOTICE
COPY licenses/ /app/licenses/
WORKDIR /app
USER 10001:10001
ENV JXPAN_LISTEN_ADDR=0.0.0.0:8789 \
    JXPAN_DB_PATH=/data/jxpan.sqlite
EXPOSE 8789
STOPSIGNAL SIGTERM
ENTRYPOINT ["/app/jxpan-rust"]
