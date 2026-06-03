#[doc = "Register `plot` reader"]
pub type R = crate::R<PLOT_SPEC>;
#[doc = "Register `plot` writer"]
pub type W = crate::W<PLOT_SPEC>;
#[doc = "Field `x` writer - x field"]
pub type X_W<'a, REG> = crate::FieldWriter<'a, REG, 12, u16>;
#[doc = "Field `y` writer - y field"]
pub type Y_W<'a, REG> = crate::FieldWriter<'a, REG, 12, u16>;
#[doc = "Field `pixel` writer - pixel field"]
pub type PIXEL_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
impl W {
    #[doc = "Bits 0:11 - x field"]
    #[inline(always)]
    pub fn x(&mut self) -> X_W<'_, PLOT_SPEC> {
        X_W::new(self, 0)
    }
    #[doc = "Bits 12:23 - y field"]
    #[inline(always)]
    pub fn y(&mut self) -> Y_W<'_, PLOT_SPEC> {
        Y_W::new(self, 12)
    }
    #[doc = "Bits 24:31 - pixel field"]
    #[inline(always)]
    pub fn pixel(&mut self) -> PIXEL_W<'_, PLOT_SPEC> {
        PIXEL_W::new(self, 24)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`plot::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`plot::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct PLOT_SPEC;
impl crate::RegisterSpec for PLOT_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`plot::R`](R) reader structure"]
impl crate::Readable for PLOT_SPEC {}
#[doc = "`write(|w| ..)` method takes [`plot::W`](W) writer structure"]
impl crate::Writable for PLOT_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets plot to value 0"]
impl crate::Resettable for PLOT_SPEC {}
