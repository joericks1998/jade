/* http/http.h — std::http runtime module ABI.
 *
 * Every verb returns a JadeDict* { status:int, body:str(TAINTED) } and never
 * raises (network/spawn failure → status 0, empty body). `headers` is a
 * JadeDict* of string→string (or NULL); `body` is the request body (POST/PUT). */
#ifndef JADE_RT_HTTP_H
#define JADE_RT_HTTP_H

void* jrt_http_get(const char* url, void* headers);
void* jrt_http_post(const char* url, const char* body, void* headers);
void* jrt_http_put(const char* url, const char* body, void* headers);
void* jrt_http_delete(const char* url, void* headers);
void* jrt_http_head(const char* url, void* headers);

#endif /* JADE_RT_HTTP_H */
