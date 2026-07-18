/* http/ — std::http runtime module.
 *
 * get/post/put/delete/head over libcurl-the-binary (exec'd directly, no shell),
 * mirroring the VM's reqwest-backed http_pkg.rs: each returns { status, body }.
 * Response bodies are external → TAINTED. Uses only libc + the public jade ABI;
 * the optional headers dict is read via jrt_coll_dict_keys / jrt_coll_dict_get.
 *
 * Note: not covered by the difftest suite (it needs the network). Kept faithful
 * to the VM's observable shape for the offline/error path (status 0, body ""). */

#include "runtime.h"
#include "http/http.h"

#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/wait.h>

/* ObjHeader dict.set with a literal string key, boxed as a TRUSTED tagged word. */
static void kd_set(void* dict, const char* key, jade_value_t val) {
    jrt_kdict_set(dict, jrt_box_str(jrt_str_dup(key, JRT_TRUSTED)), val);
}

/* Build one "Key: Value" header line (heap, caller frees) from a dict entry. */
static char* header_line(const char* k, const char* v) {
    size_t kl = strlen(k), vl = strlen(v);
    char* s = malloc(kl + 2 + vl + 1);
    if (!s) return NULL;
    memcpy(s, k, kl);
    s[kl] = ':'; s[kl+1] = ' ';
    memcpy(s + kl + 2, v, vl);
    s[kl + 2 + vl] = '\0';
    return s;
}

/* Core: exec curl with the given method/body/headers, capture stdout. Returns
 * the { status, body } dict. `body` (NULL for none) is streamed to curl's stdin
 * via --data-binary @- so arbitrary/binary content is safe. */
static jade_value_t http_request(const char* method, const char* url,
                                 const char* body, void* headers) {
    void* dict = jrt_kdict_new();
    kd_set(dict, "status", jrt_box_int(0));
    kd_set(dict, "body", jrt_box_str(jrt_str_dup("", JRT_TAINTED)));
    if (!url) return jrt_box_ptr(dict);

    int is_head = (strcmp(method, "HEAD") == 0);

    /* Flatten the headers ObjHeader dict into "K: V" lines (sorted keys,
     * deterministic — jrt_coll_dict_keys returns them sorted). */
    char** hlines = NULL;
    int64_t nh = 0;
    if (headers) {
        void* keys = jrt_coll_dict_keys(headers);
        nh = jrt_coll_array_len(keys);
        if (nh > 0) {
            hlines = calloc((size_t)nh, sizeof(char*));
            for (int64_t i = 0; i < nh; i++) {
                int64_t kv = jrt_coll_array_get(keys, i);
                const char* k = (const char*)jrt_unbox_ptr((jade_value_t)kv);
                int64_t vv = 0;
                const char* v = (jrt_coll_dict_get(headers, k, &vv) && jrt_is_str((jade_value_t)vv))
                              ? (const char*)jrt_unbox_ptr((jade_value_t)vv) : "";
                hlines[i] = header_line(k, v);
            }
        }
    }

    /* argv: curl -sS [-o /dev/null] -X M [-H h]... [--data-binary @-]
     *       -w "\nJADE_STATUS:%{http_code}" -- url  (+NULL) */
    size_t cap = 12 + (size_t)nh * 2;
    char** argv = calloc(cap, sizeof(char*));
    size_t a = 0;
    argv[a++] = (char*)"curl";
    argv[a++] = (char*)"-sS";
    if (is_head) { argv[a++] = (char*)"-o"; argv[a++] = (char*)"/dev/null"; }
    argv[a++] = (char*)"-X"; argv[a++] = (char*)method;
    for (int64_t i = 0; i < nh; i++) {
        if (hlines && hlines[i]) { argv[a++] = (char*)"-H"; argv[a++] = hlines[i]; }
    }
    if (body) { argv[a++] = (char*)"--data-binary"; argv[a++] = (char*)"@-"; }
    argv[a++] = (char*)"-w"; argv[a++] = (char*)"\nJADE_STATUS:%{http_code}";
    argv[a++] = (char*)"--"; argv[a++] = (char*)url;
    argv[a]   = NULL;

    int outp[2], inp[2];
    if (pipe(outp) != 0) { goto cleanup; }
    if (body) { if (pipe(inp) != 0) { close(outp[0]); close(outp[1]); goto cleanup; } }

    pid_t pid = fork();
    if (pid < 0) {
        close(outp[0]); close(outp[1]);
        if (body) { close(inp[0]); close(inp[1]); }
        goto cleanup;
    }
    if (pid == 0) {
        dup2(outp[1], STDOUT_FILENO);
        close(outp[0]); close(outp[1]);
        if (body) { dup2(inp[0], STDIN_FILENO); close(inp[0]); close(inp[1]); }
        int devnull = open("/dev/null", O_WRONLY);
        if (devnull >= 0) { dup2(devnull, STDERR_FILENO); close(devnull); }
        execvp("curl", argv);
        _exit(127);
    }

    /* Parent. */
    close(outp[1]);
    if (body) {
        close(inp[0]);
        size_t bl = strlen(body), off = 0;
        while (off < bl) {
            ssize_t w = write(inp[1], body + off, bl - off);
            if (w <= 0) break;
            off += (size_t)w;
        }
        close(inp[1]);
    }

    size_t bufcap = 4096, len = 0;
    char* buf = malloc(bufcap);
    if (buf) {
        ssize_t nr;
        while ((nr = read(outp[0], buf + len, bufcap - len - 1)) > 0) {
            len += (size_t)nr;
            if (len + 1 >= bufcap) {
                bufcap *= 2;
                char* nb = realloc(buf, bufcap);
                if (!nb) { free(buf); buf = NULL; break; }
                buf = nb;
            }
        }
    }
    close(outp[0]);
    int wstatus = 0;
    waitpid(pid, &wstatus, 0);
    int curl_rc = WIFEXITED(wstatus) ? WEXITSTATUS(wstatus) : -1;

    /* curl exits 0 for any HTTP response — including 4xx/5xx, which are reported
     * via the `status` field (a 404 is `{status: 404}`, not an error). A non-zero
     * exit is a TRANSPORT failure (DNS, connect, timeout, TLS), so raise an
     * explicit error instead of silently returning {status: 0}. Mirrors the VM,
     * which raises on a failed request. */
    if (curl_rc != 0) {
        free(buf);
        const char* reason =
            curl_rc == 6   ? "could not resolve host" :
            curl_rc == 7   ? "could not connect to host" :
            curl_rc == 28  ? "request timed out" :
            curl_rc == 35  ? "TLS handshake failed" :
            curl_rc == 127 ? "curl executable not found" : "request failed";
        char msg[1124];
        snprintf(msg, sizeof msg, "http %s '%s': %s (curl exit %d)",
                 method, url, reason, curl_rc);
        if (hlines) { for (int64_t i = 0; i < nh; i++) free(hlines[i]); free(hlines); }
        free(argv);
        jade_exc_throw_typed(jrt_box_str(jrt_str_dup(msg, JRT_TRUSTED)), NULL);
        return jrt_box_ptr(dict); /* unreachable: throw longjmps or exits */
    }

    if (buf) {
        buf[len] = '\0';
        char* marker = strstr(buf, "\nJADE_STATUS:");
        int64_t status = 0;
        size_t body_len = len;
        if (marker) { body_len = (size_t)(marker - buf); status = atoll(marker + 13); }
        char* rbody = jrt_str_new(body_len, JRT_TAINTED);
        if (body_len > 0) memcpy(rbody, buf, body_len);
        free(buf);
        kd_set(dict, "status", jrt_box_int(status));
        kd_set(dict, "body", jrt_box_str(rbody));
    }

cleanup:
    if (hlines) { for (int64_t i = 0; i < nh; i++) free(hlines[i]); free(hlines); }
    free(argv);
    return jrt_box_ptr(dict);
}

jade_value_t jrt_http_get(const char* url, void* headers) {
    return http_request("GET", url, NULL, headers);
}
jade_value_t jrt_http_post(const char* url, const char* body, void* headers) {
    return http_request("POST", url, body ? body : "", headers);
}
jade_value_t jrt_http_put(const char* url, const char* body, void* headers) {
    return http_request("PUT", url, body ? body : "", headers);
}
jade_value_t jrt_http_delete(const char* url, void* headers) {
    return http_request("DELETE", url, NULL, headers);
}
jade_value_t jrt_http_head(const char* url, void* headers) {
    return http_request("HEAD", url, NULL, headers);
}
