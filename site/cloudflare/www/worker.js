// www.skink.cc -> skink.cc, keeping the path and query.
export default {
  fetch(request) {
    const url = new URL(request.url);
    url.hostname = 'skink.cc';
    return Response.redirect(url.toString(), 301);
  },
};
